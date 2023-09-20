use std::{
    path::{
        Path,
        PathBuf,
    },
    process::{
        Command,
        ExitStatus,
    },
    fmt,
};
use regex::Regex;
use log;
use ffprobe;
use lazy_static::lazy_static;

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.kind)
    }
}

#[derive(Debug)]
pub enum ErrorKind {
    NoAvailableVideoStream,
    VersionCheckCommandProcessFailed(String),
    VersionOutputNotMatched(String),
    VersionNotValidInteger(String),
    NotSupportedCommandVersion(u8, u8),
    FfmpegCommandProcessFailed(String),
    FfmpegCommandExitAbnormally(ExitStatus, String),
    AbAv1CommandProcessFailed(PathBuf, String),
    InvalidAbAv1Output(PathBuf, String),
    UnknownAbAv1ErrorMessage(PathBuf, String),
}

const AB_AV1_CMD_STR: &str = "ab-av1";
const FFMPEG_CMD_STR: &str = "ffmpeg";

lazy_static! {
    static ref FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX: Regex = Regex::new(r"^ffmpeg\s+version\s+(\d+)\.(\d+)\b").unwrap();
    static ref AB_AV1_STDOUT_RETRIEVE_VERSION_REGEX: Regex = Regex::new(r"^ab-av1\s+(\d+)\.(\d+).\d\b").unwrap();
    static ref AB_AV1_STDOUT_RETRIEVE_CRF_REGEX: Regex = Regex::new(r"^\s*crf\s+(\d+)").unwrap();
    static ref AB_AV1_STDERR_CHECK_GOOD_CRF_NOT_FOUND_REGEX: Regex = Regex::new(r"Failed to find a suitable crf\s*$").unwrap();
}

struct InputFile {
    path: PathBuf,
    width: i64,
    height: i64,
    has_audio: bool,
}

pub(crate) fn encode_best_effort(input_video_paths: Vec<PathBuf>, output_video_path: impl AsRef<Path>, enough_vmaf: u8, min_crf: u8) -> Result<(), Error> {
    let output_video_path = output_video_path.as_ref();

    check_command(6, 0, FFMPEG_CMD_STR, &["-version"], &FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX)?;
    check_command(0, 7, AB_AV1_CMD_STR, &["--version"], &FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX)?;

    let mut max_width = 0; 
    let mut max_height = 0;
    let mut input_files = Vec::new();
    let mut best_resolution = 0;
    let mut best_input_video_path = Default::default();

    for path in input_video_paths {
        let streams = match ffprobe::ffprobe(&path) {
            Ok(ffprobe::FfProbe { streams, .. }) => streams,
            Err(err) => {
                log::warn!("Video file not support, ignored: {:} ({:})", path.display(), err);
                continue;
            },
        };

        let Some(video_stream) = get_first_video_stream(&streams) else {
            log::warn!("No video stream in file, ignored: {:}", path.display());
            continue;
        };

        let (Some(width), Some(height)) = (video_stream.width, video_stream.height) else {
            log::warn!("Couldn't get video resolution, ignored: {:}", path.display());
            continue;
        };

        if width < 0 || height < 0 {
            log::warn!("Invalid resolution, ignored: {:} ({:}, {:})", path.display(), width, height);
            continue;
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
            best_input_video_path = path.clone();
        }

        input_files.push(InputFile { path, width, height, has_audio });
    }

    let input_count = input_files.len();
    let needs_concatenation = match input_count {
        0 => return Err(Error { kind: ErrorKind::NoAvailableVideoStream }),
        1 => false,
        _ => true,
    };

    log::info!(
        "All files: max_width = {:}, max_height = {:}, best_resolution = {:}, best_input_video_path = {:}",
        max_width, max_height, best_resolution,
        best_input_video_path.display(),
    );

    let mut ffmpeg_cmd = Command::new(FFMPEG_CMD_STR);
    ffmpeg_cmd.arg("-y");

    for input_file in &input_files {
        ffmpeg_cmd.arg("-i");
        ffmpeg_cmd.arg(&input_file.path);
    }

    if needs_concatenation {
        let mut filter_code = String::new();
        let mut concat_input_part_filter_code = String::new();
        for (index, input_file) in input_files.iter().enumerate() {
            let part_filter_code = if input_file.width == max_width && input_file.height == max_height {
                "null".to_string()
            } else if input_file.width * max_height == input_file.height * max_width {
                // same aspect ratio
                format!("scale={:}:{:}", max_width, max_height)
            } else {
                format!("scale={0:}:{1:}:force_original_aspect_ratio=decrease,pad={0:}:{1:}:(ow-iw)/2:(oh-ih)/2", max_width, max_height)
            };

            let filter_code_statement = format!("[{0:}:v:0]{1:}[v{0:}];", index, part_filter_code);
            filter_code.push_str(&filter_code_statement);

            log::info!("Add filter: {:}", filter_code_statement);

            concat_input_part_filter_code.push_str(&format!("[v{0:}]", index));
            if input_file.has_audio {
                concat_input_part_filter_code.push_str(&format!("[{0:}:a:0]", index));
            }
        }
        let filter_code_statement = format!("{:}concat=n={:}:v=1:a=1[vout][aout]", concat_input_part_filter_code, input_count);
        log::info!("Add filter: {:}", filter_code_statement);
        filter_code.push_str(&filter_code_statement);

        ffmpeg_cmd.args(["-filter_complex", &filter_code, "-map", "[vout]", "-map", "[aout]"]);
    }

    log::info!("Start search crf: {:} vmaf={:} crf={:}", best_input_video_path.display(), enough_vmaf, min_crf);
    let (best_crf, found) = get_best_crf(best_input_video_path, enough_vmaf, min_crf)?;
    if found {
        log::info!("Crf found: {:}", best_crf);
    } else {
        log::info!("Suitable crf not found use min: {:}", best_crf);
    };

    let best_crf = best_crf.to_string();
    ffmpeg_cmd.args([
        "-c:v", "libsvtav1",
        "-crf", &best_crf,
        "-pix_fmt", "yuv420p10le",
        "-preset", "8",
    ]);

    ffmpeg_cmd.arg(&output_video_path);

    log::info!("Start ffmpeg: {:?}", ffmpeg_cmd);
    let output = match ffmpeg_cmd.output() {
        Ok(output) => output,
        Err(err) => return Err(Error { kind: ErrorKind::FfmpegCommandProcessFailed(err.to_string()) }),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(Error { kind: ErrorKind::FfmpegCommandExitAbnormally(output.status, stderr) });
    }

    Ok(())
}

fn check_command(expected_major_version: u8, min_minor_version: u8, cmd: &str, args: &[&str], re: &Regex) -> Result<(), Error> {
    let mut cmd = Command::new(cmd);
    cmd.args(args);
    let output = match cmd.output() {
        Ok(output) => output,
        Err(err) => return Err(Error { kind: ErrorKind::VersionCheckCommandProcessFailed(err.to_string()) }),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let Some(caps) = re.captures(&stdout) else {
        return Err(Error { kind: ErrorKind::VersionOutputNotMatched(stdout) });
    };
    if caps.len() < 2 {
        return Err(Error { kind: ErrorKind::VersionOutputNotMatched(stdout) });
    }
    let Ok(major_version) = caps[1].parse::<u8>() else {
        return Err(Error { kind: ErrorKind::VersionNotValidInteger(caps[1].to_string()) });
    };

    let Ok(minor_version) = caps[2].parse::<u8>() else {
        return Err(Error { kind: ErrorKind::VersionNotValidInteger(caps[2].to_string()) });
    };

    if expected_major_version != major_version || minor_version < min_minor_version {
        return Err(Error { kind: ErrorKind::NotSupportedCommandVersion(major_version, minor_version) });
    };

    Ok(())
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

fn get_best_crf(video_path: impl AsRef<Path>, enough_vmaf: u8, min_crf: u8) -> Result<(u8, bool), Error> {
    let video_path = video_path.as_ref();

    let mut ab_av1_cmd = Command::new(AB_AV1_CMD_STR);
    ab_av1_cmd.args([
        "crf-search",
        "--min-vmaf", &enough_vmaf.to_string(),
        "--min-crf", &(min_crf + 1).to_string(),
        "--max-encoded-percent", "100",
        "--enc", "fps_mode=passthrough",
        "--enc", "dn",
        "--input",
    ]).arg(&video_path);

    let output = match ab_av1_cmd.output() {
        Ok(output) => output,
        Err(err) => return Err(Error { kind: ErrorKind::AbAv1CommandProcessFailed(video_path.into(), err.to_string()) }),
    };

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let Some(caps) = AB_AV1_STDOUT_RETRIEVE_CRF_REGEX.captures(&stdout) else {
            return Err(Error { kind: ErrorKind::InvalidAbAv1Output(video_path.into(), stdout) });
        };
        let Ok(crf) = caps[1].parse::<u8>() else {
            return Err(Error { kind: ErrorKind::InvalidAbAv1Output(video_path.into(), stdout) });
        };
        Ok((crf, true))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !AB_AV1_STDERR_CHECK_GOOD_CRF_NOT_FOUND_REGEX.is_match(&stderr) {
            return Err(Error { kind: ErrorKind::UnknownAbAv1ErrorMessage(video_path.into(), stderr) });
        }
        // if failed with not found good crf, then max crf
        Ok((min_crf, false))
    }
}

