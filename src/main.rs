use std::{
    env,
    path::{
        Path,
        PathBuf,
    },
    process::Command,
};
use google_cloud_storage::{
    client::{
        Client,
        ClientConfig,
    },
    http::objects::{
        download::Range,
        get::GetObjectRequest,
    },
};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
};
use futures::stream::StreamExt;
use ffprobe;
use regex::Regex;

#[tokio::main]
async fn main() {
    let bucket = match env::var("BUCKET") {
        Ok(bucket) => bucket, 
        Err(err) => panic!("BUCKET env var not set or invalid utf-8: {:}", err),
    };
    let enough_vmaf = match env::var("ENOUGH_VMAF") {
        Ok(enough_vmaf) => match enough_vmaf.parse::<u8>() {
            Ok(enough_vmaf) => enough_vmaf,
            Err(err) => panic!("ENOUGH_VMAF couldn't parse as unsigned int: {:}", err),
        }, 
        Err(err) => panic!("ENOUGH_VMAF env var not set or invalid utf-8: {:}", err),
    };
    let min_crf = match env::var("MIN_CRF") {
        Ok(min_crf) => match min_crf.parse::<u8>() {
            Ok(min_crf) => min_crf,
            Err(err) => panic!("MIN_CRF couldn't parse as unsigned int: {:}", err),
        }, 
        Err(err) => panic!("MIN_CRF env var not set or invalid utf-8: {:}", err),
    };

    let mut args = env::args().skip(1);

    let Some(output_object_id) = args.next() else {
        panic!("No output gcs object id given");
    };
    let output_object_path = Path::new("output").join(&output_object_id);

    let object_ids = args.collect::<Vec<_>>();
    let object_count = object_ids.len();
    if object_ids.len() == 0 {
        panic!("No gcs object id given");
    }

    let config = ClientConfig::default().with_auth().await.expect("Couldn't auth");
    let object_paths = download_objects(config, bucket, object_ids).await;

    struct PartFile {
        object_path: PathBuf,
        width: i64,
        height: i64,
        has_audio: bool,
    }

    let mut max_width = 0; 
    let mut max_height = 0;
    let mut part_files = Vec::new();
    let mut best_resolution = 0;
    let mut best_object_path = None;

    for object_path in object_paths {
        match ffprobe::ffprobe(&object_path) {
            Ok(ffprobe::FfProbe { streams, .. }) => {
                let Some(video_stream) = get_first_video_stream(&streams) else {
                    panic!("Couldn't get video stream: {:}", object_path.display())
                };
                let (Some(width), Some(height)) = (video_stream.width, video_stream.height) else {
                    panic!("Couldn't get video resolution: {:}", object_path.display())
                };
                if width < 0 || height < 0 {
                    panic!("Invalid resolution: ({:}, {:})", width, height);
                };
                let has_audio = has_audio_stream(&streams);

                let resolution = width * height;
                if max_width < width {
                    max_width = width;
                }
                if max_height < height {
                    max_height = height;
                }
                if best_resolution < resolution {
                    best_resolution = resolution;
                    best_object_path = Some(object_path.clone());
                }

                part_files.push(PartFile { object_path, width, height, has_audio });
            },
            Err(err) => panic!("Couldn't analyze file with ffprobe: {:} ({:})", object_path.display(), err),
        }
    }
    let Some(best_object_path) = best_object_path else {
        panic!("best_object_path must be found in this context")
    };

    println!("All files: max_width = {:}, max_height = {:}, best_resolution = {:}, best_object_path = {:}", max_width, max_height, best_resolution, best_object_path.display());

    let mut ffmpeg_args = Vec::new();
    ffmpeg_args.push("-y");
    for part_file in &part_files {
        ffmpeg_args.push("-i");
        ffmpeg_args.push(part_file.object_path.to_str().expect("Unexpected nun unicode path string"));
    }

    let filter_code = if 1 < object_count {
        let mut filter_code = String::new();
        let mut concat_input_part_filter_code = String::new();
        for (index, part_file) in part_files.iter().enumerate() {
            let part_filter_code = if part_file.width == max_width && part_file.height == max_height {
                "null".to_string()
            } else if part_file.width * max_height == part_file.height * max_width {
                // same aspect ratio
                format!("scale={:}:{:}", max_width, max_height)
            } else {
                format!("scale={0:}:{1:}:force_original_aspect_ratio=decrease,pad={0:}:{1:}:(ow-iw)/2:(oh-ih)/2", max_width, max_height)
            };

            let filter_code_statement = format!("[{0:}:v:0]{1:}[v{0:}];", index, part_filter_code);
            filter_code.push_str(&filter_code_statement);
            println!("Add filter: {:}", filter_code_statement);
            concat_input_part_filter_code.push_str(&format!("[v{0:}]", index));
            if part_file.has_audio {
                concat_input_part_filter_code.push_str(&format!("[{0:}:a:0]", index));
            }
        }
        let filter_code_statement = format!("{:}concat=n={:}:v=1:a=1[vout][aout]", concat_input_part_filter_code, object_count);
        println!("Add filter: {:}", filter_code_statement);
        filter_code.push_str(&filter_code_statement);
        Some(filter_code)
    } else {
        None
    };

    if let Some(ref filter_code) = filter_code {
        ffmpeg_args.extend_from_slice(&[
            "-filter_complex",
            &filter_code,
        ]);
    }

    println!("Start search crf: {:} vmaf={:} crf={:}", best_object_path.display(), enough_vmaf, min_crf);
    let best_crf = get_best_crf(best_object_path, enough_vmaf, min_crf);
    let best_crf = best_crf.to_string();
    ffmpeg_args.extend_from_slice(&[
        "-c:v", "libsvtav1",
        "-crf", &best_crf,
        "-pix_fmt", "yuv420p10le",
        "-preset", "8",
    ]);

    // filter output 
    if filter_code.is_some() {
        ffmpeg_args.extend_from_slice(&["-map", "[vout]", "-map", "[aout]"]);
    }

    ffmpeg_args.push(&output_object_path.to_str().expect("Unexpected nun unicode path string"));

    println!("Start ffmpeg: {:}", ffmpeg_args.join(" "));
    let output = Command::new("ffmpeg").args(ffmpeg_args).output().expect("Ffmpeg process failed to start");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(exit_code) = output.status.code() {
            panic!("Ffmpeg process finished with exit code: {:}\nError: {:}", exit_code, stderr)
        } else {
            panic!("Ffmpeg process finished with no exit code: {:}", stderr)
        }
    }
}

async fn download_objects(config: ClientConfig, bucket: String, object_ids: Vec<String>) -> Vec<PathBuf> {
    let client = Client::new(config);
    let mut object_paths = Vec::new();
    for object_id in object_ids.into_iter() {
        let object_path = Path::new("data").join(&object_id);
        download_object(&client, bucket.clone(), object_id, &object_path).await;
        object_paths.push(object_path);
    }
    object_paths
}

async fn download_object(client: &Client, bucket: String, object_id: String, save_path: impl AsRef<Path>) {
    let Ok(mut object_stream) = client.download_streamed_object(&GetObjectRequest {
        bucket: bucket,
        object: object_id.clone(),
        ..Default::default()
    }, &Range::default()).await else {
        panic!("Couldn't get object stream: {:}", object_id);
    };

    let save_path = save_path.as_ref();
    let Ok(mut file) = File::create(save_path.clone()).await else {
        panic!("Couldn't create the save_path: {:}", save_path.display());
    };

    while let Some(item) = object_stream.next().await {
        let Ok(bytes) = item else {
            panic!("Couldn't receive bytes in object: {:}", object_id);
        };
        if let Err(err) = file.write_all(&bytes).await {
            panic!("Couldn't write bytes to file: {:} ({:})", save_path.display(), err);
        };
    }
}

fn get_first_video_stream<'a>(streams: &'a Vec<ffprobe::Stream>) -> Option<&'a ffprobe::Stream> {
    for stream in streams {
        if stream.codec_type == Some("video".to_string()) {
            return Some(stream);
        }
    }
    None
}

fn has_audio_stream(streams: &Vec<ffprobe::Stream>) -> bool {
    for stream in streams {
        if stream.codec_type == Some("audio".to_string()) {
            return true
        }
    }
    false
}

fn get_best_crf(best_object_path: impl AsRef<Path>, enough_vmaf: u8, min_crf: u8) -> u8 {
    let output = Command::new("ab-av1").args([
        "crf-search",
        "--min-vmaf", &enough_vmaf.to_string(),
        "--min-crf", &(min_crf + 1).to_string(),
        "--max-encoded-percent", "100",
        "--enc", "fps_mode=passthrough",
        "--enc", "dn",
        "--input", &best_object_path.as_ref().to_str().expect("Unexpected nun unicode path string"),
    ]).output().expect("ab-av1 process failed");

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let re = Regex::new(r"^\s*crf\s+(\d+)").unwrap();
        let Some(caps) = re.captures(&stdout) else {
            panic!("Invalid ab-av1 output: {:}", stdout);
        };
        let Ok(crf) = caps[1].parse::<u8>() else {
            panic!("Invalid crf number in ab-av1 output: {:}", stdout);
        };

        println!("Crf found: {:}", crf);
        crf
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let re = Regex::new(r"Failed to find a suitable crf\s*$").unwrap();
        if !re.is_match(&stderr) {
            panic!("ab-av1 failed with unknown error message: {:}", stderr);
        }

        // if failed with not found good crf, then max crf
        println!("Suitable crf not found use min: {:}", min_crf);
        min_crf
    }
}

