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

const AB_AV1_CMD_STR: &str = "ab-av1";
const FFMPEG_CMD_STR: &str = "ffmpeg";
const MAX_CRF: u8 = 55;

const FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX_SOURCE: &str = r"^ffmpeg\s+version\s+(\d+)\.(\d+)\b";
const AB_AV1_STDOUT_RETRIEVE_VERSION_REGEX_SOURCE: &str = r"^ab-av1\s+(\d+)\.(\d+).\d\b";

lazy_static! {
    static ref FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX: Regex = Regex::new(FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX_SOURCE).unwrap();
    static ref AB_AV1_STDOUT_RETRIEVE_VERSION_REGEX: Regex = Regex::new(AB_AV1_STDOUT_RETRIEVE_VERSION_REGEX_SOURCE).unwrap();

    static ref AB_AV1_STDOUT_RETRIEVE_CRF_REGEX: Regex = Regex::new(r"^\s*crf\s+(\d+)\s+VMAF\s+(\d+(?:\.\d+)?)").unwrap();
    static ref AB_AV1_STDERR_CHECK_GOOD_CRF_NOT_FOUND_REGEX: Regex = Regex::new(r"Failed to find a suitable crf\s*$").unwrap();
}

#[derive(Debug, PartialEq)]
pub struct Error {
    kind: ErrorKind,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.kind)
    }
}

#[derive(Debug, PartialEq)]
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

#[cfg(test)]
mod test_error {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!("NoAvailableVideoStream".to_string(), (Error { kind: ErrorKind::NoAvailableVideoStream }).to_string());

        // just coverage for debug trait
        assert!(0 < format!("{:?}", Error { kind: ErrorKind::NoAvailableVideoStream }).len());

        // just coverage for partial eq trait
        assert_eq!(Error { kind: ErrorKind::NoAvailableVideoStream }, Error { kind: ErrorKind::NoAvailableVideoStream });
    }
}

#[derive(Debug)]
struct InputFile {
    path: PathBuf,
    width: i64,
    height: i64,
    alternative_null_audio_duration: Option<f64>,
}

#[cfg(test)]
mod test_input_file {
    use super::*;

    #[test]
    fn it_works() {
        // just coverage for debug trait
        assert!(0 < format!("{:?}", InputFile { path: PathBuf::from("."), width: 1, height: 2, alternative_null_audio_duration: None }).len());
    }
}

pub(crate) fn encode_best_effort(input_video_paths: Vec<PathBuf>, output_video_path: impl AsRef<Path>, enough_vmaf: u8, min_crf: u8) -> Result<(u8, Option<f64>), Error> {
    encode_best_effort_impl(FFMPEG_CMD_STR, input_video_paths, output_video_path, enough_vmaf, min_crf)
}

// separate impl for test
fn encode_best_effort_impl(cmd_str: &str, input_video_paths: Vec<PathBuf>, output_video_path: impl AsRef<Path>, enough_vmaf: u8, min_crf: u8) -> Result<(u8, Option<f64>), Error> {
    log::trace!("encode_best_effort(): {:?}", (&input_video_paths, output_video_path.as_ref(), enough_vmaf, min_crf));
    let output_video_path = output_video_path.as_ref();

    check_command(6, 0, FFMPEG_CMD_STR, &["-version"], &FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX)?;
    check_command(0, 7, AB_AV1_CMD_STR, &["--version"], &AB_AV1_STDOUT_RETRIEVE_VERSION_REGEX)?;

    let input_files = input_video_paths.into_iter()
        .filter_map(analyze_video_file)
        .collect::<Vec<_>>();

    let needs_concatenation = match input_files.len() {
        0 => {
            log::trace!("encode_best_effort() -> Error(NoAvailableVideoStream): {:?}", (&input_files));
            return Err(Error { kind: ErrorKind::NoAvailableVideoStream });
        },
        1 => false,
        _ => true,
    };

    let mut ffmpeg_cmd = Command::new(cmd_str);
    ffmpeg_cmd.arg("-y");

    for input_file in &input_files {
        ffmpeg_cmd.arg("-i");
        ffmpeg_cmd.arg(&input_file.path);
    }

    if needs_concatenation {
        let filter_code = get_avfilter_code(&input_files);
        ffmpeg_cmd.args(["-filter_complex", &filter_code, "-map", "[vout]", "-map", "[aout]"]);
    }

    assert!(0 < input_files.len());
    let best_input_file = input_files.iter().max_by_key(|input_file| input_file.width * input_file.height).expect("must not be none, because vec is not empty");
    

    log::info!("Start search crf: {:} vmaf={:} crf={:}", best_input_file.path.display(), enough_vmaf, min_crf);
    let (best_crf, predicted_vmaf) = get_best_crf(&best_input_file.path, enough_vmaf, min_crf)?;
    if let Some(predicted_vmaf) = predicted_vmaf {
        log::info!("Crf found: {:} (vmaf={:})", best_crf, predicted_vmaf);
    } else {
        log::info!("Suitable crf not found use min: {:}", best_crf);
    };

    let best_crf_str = best_crf.to_string();
    ffmpeg_cmd.args([
        "-c:v", "libsvtav1",
        "-crf", &best_crf_str,
        "-pix_fmt", "yuv420p10le",
        "-preset", "8",
    ]);

    ffmpeg_cmd.arg(&output_video_path);

    log::info!("Start ffmpeg: {:?}", ffmpeg_cmd);
    let output = match ffmpeg_cmd.output() {
        Ok(output) => output,
        Err(err) => {
            log::trace!("encode_best_effort() -> Error(FfmpegCommandProcessFailed({:?})): {:?}", &err, (&ffmpeg_cmd));
            return Err(Error { kind: ErrorKind::FfmpegCommandProcessFailed(err.to_string()) });
        },
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        log::trace!("encode_best_effort() -> Error(FfmpegCommandExitAbnormally({:?}, {:?})): {:?}", &output.status, &stderr, (&ffmpeg_cmd));
        return Err(Error { kind: ErrorKind::FfmpegCommandExitAbnormally(output.status, stderr) });
    }

    log::trace!("encode_best_effort() -> Ok");
    Ok((best_crf, predicted_vmaf))
}

#[cfg(test)]
mod test_encode_best_effort {
    use super::*;
    use std::env;

    #[test]
    fn it_works() {
        let test_cases = vec![
            (vec!["va-300x400.mp4"], "va.mp4", 0, MAX_CRF - 2, true, 1.0, MAX_CRF, true),
            (vec!["va-300x400.mp4", "va-300x400.mp4"], "va-va.mp4", 0, MAX_CRF - 2, true, 2.0, MAX_CRF, true),
            (vec!["v-300x400.mp4"], "v.mp4", 0, MAX_CRF - 2, true, 1.0, MAX_CRF, true),
            (vec!["v-300x400.mp4", "v-300x400.mp4"], "v-v.mp4", 0, MAX_CRF - 2, true, 2.0, MAX_CRF, true),
            (vec!["va-300x400.mp4", "v-300x400.mp4"], "va-v.mp4", 0, MAX_CRF - 2, true, 2.0, MAX_CRF, true),
            (vec!["v-300x400.mp4", "va-300x400.mp4"], "v-va.mp4", 0, MAX_CRF - 2, true, 2.0, MAX_CRF, true),
            (vec!["v-300x400.mp4", "va-300x400.mp4", "v-300x400.mp4"], "v-va-v.mp4", 0, MAX_CRF - 2, true, 3.0, MAX_CRF, true),
            (vec!["va-300x400.mp4", "v-300x400.mp4", "va-300x400.mp4"], "va-v-va.mp4", 0, MAX_CRF - 2, true, 3.0, MAX_CRF, true),
            (vec!["a.mp4"], "a.mp4", 0, MAX_CRF - 2, false, 0.0, 0, false),
        ];

        evauate_test_cases(test_cases);
    }

    #[test]
    fn it_ignores_not_supported() {
        let test_cases = vec![
            (vec!["invalid.mp4", "va-300x400.mp4", "invalid.mp4", "va-300x400.mp4", "invalid.mp4"], "it_ignores_not_supported.mp4", 0, MAX_CRF - 2, true, 2.0, MAX_CRF, true),
        ];
        evauate_test_cases(test_cases);
    }

    #[test]
    fn it_can_use_min_crf() {
        let test_cases = vec![
            (vec!["invalid.mp4", "va-300x400.mp4", "invalid.mp4", "va-300x400.mp4", "invalid.mp4"], "it_can_use_min_crf-0.mp4", 0, MAX_CRF - 2, true, 2.0, MAX_CRF, true),
            (vec!["invalid.mp4", "va-300x400.mp4", "invalid.mp4", "va-300x400.mp4", "invalid.mp4"], "it_can_use_min_crf-1.mp4", 100, MAX_CRF - 2, true, 2.0, MAX_CRF - 2, false),
        ];
        evauate_test_cases(test_cases);
    }

    #[test]
    fn it_fails_when_ffmpeg_command_failed() {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");
        let output_dir_path = root_path.join("output");

        assert!(match encode_best_effort_impl("__command_not_found__", vec![video_dir_path.join("va-300x400.mp4")], output_dir_path.join("it_fails_when_ffmpeg_command_failed.mp4"), 0, MAX_CRF - 2) {
            Err(Error { kind: ErrorKind::FfmpegCommandProcessFailed(_) }) => true, _ => false,
        });
        assert!(match encode_best_effort_impl("false", vec![video_dir_path.join("va-300x400.mp4")], output_dir_path.join("it_fails_when_ffmpeg_command_failed.mp4"), 0, MAX_CRF - 2) {
            Err(Error { kind: ErrorKind::FfmpegCommandExitAbnormally(_, _) }) => true, _ => false,
        });
    }

    fn evauate_test_cases(test_cases: Vec<(Vec<&str>, &str, u8, u8, bool, f64, u8, bool)>) {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");
        let output_dir_path = root_path.join("output");

        for (input_filenames, output_filename, vmaf, crf, expected_result, expected_duration, expected_crf, expected_crf_found) in test_cases {
            let input_paths = input_filenames.iter().map(|filename| { video_dir_path.join(filename) }).collect::<Vec<_>>();
            let output_path = output_dir_path.join(&output_filename);
            let (actual_result, actual_crf, actual_crf_found) = match encode_best_effort(input_paths, &output_path, vmaf, crf) {
                Ok((crf, predicted_vmaf)) => {
                    (true, crf, predicted_vmaf.is_some())
                },
                Err(err) => {
                    log::trace!("test_encode_best_effort() case {:?} error {:?}", (input_filenames, output_filename, vmaf, crf, expected_result), err);
                    (false, 0, false)
                },
            };
            assert_eq!(actual_result, expected_result);
            assert_eq!(actual_crf_found, expected_crf_found);
            assert_eq!(actual_crf, expected_crf);
            if actual_result {
                let ffprobe::FfProbe { format, streams } = ffprobe::ffprobe(&output_path).unwrap();

                let video_stream = get_first_video_stream(&streams).unwrap();
                let actual_duration = get_stream_duration(&video_stream, &format).unwrap();
                assert_eq!((actual_duration * 10.0).round(), expected_duration * 10.0);

                if let Some(audio_stream) = get_first_audio_stream(&streams) {
                    let actual_duration = get_stream_duration(&audio_stream, &format).unwrap();
                    assert_eq!((actual_duration * 10.0).round(), expected_duration * 10.0);
                };
            }
        }
    }

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
    assert!(caps.len() >= 2);

    let major_version = parse_number::<u8, _>(&caps[1], Error { kind: ErrorKind::VersionNotValidInteger(caps[1].to_string()) })?;
    let minor_version = parse_number::<u8, _>(&caps[2], Error { kind: ErrorKind::VersionNotValidInteger(caps[2].to_string()) })?;

    if expected_major_version != major_version || minor_version < min_minor_version {
        return Err(Error { kind: ErrorKind::NotSupportedCommandVersion(major_version, minor_version) });
    };

    Ok(())
}

#[cfg(test)]
mod test_check_command {
    use super::*;

    #[test]
    fn it_works() {
        let test_cases = [
            (6, 0, "ffmpeg", "-version", FFMPEG_STDOUT_RETRIEVE_VERSION_REGEX_SOURCE, true),
            (0, 7, "ab-av1", "--version", AB_AV1_STDOUT_RETRIEVE_VERSION_REGEX_SOURCE, true),
            (0, 0, "__command_not_found__", "__unused__", r".", false),
            (0, 0, "echo", "0.0", r"__not_matched__", false),
            (0, 0, "echo", "0.0", r"^(\d+)\.(\d+)", true),
            (5, 5, "echo", "5.5", r"^(\d+)\.(\d+)", true),
            (5, 5, "echo", "4.5", r"^(\d+)\.(\d+)", false),
            (5, 5, "echo", "6.5", r"^(\d+)\.(\d+)", false),
            (5, 5, "echo", "5.6", r"^(\d+)\.(\d+)", true),
            (5, 5, "echo", "5.4", r"^(\d+)\.(\d+)", false),
            (255, 255, "echo", "255.256", r"^(\d+)\.(\d+)", false), // too big
            (255, 255, "echo", "256.255", r"^(\d+)\.(\d+)", false), // too big
            (255, 255, "echo", "255.255", r"^(\d+)\.(\d+)", true),
        ];

        for (expected_major_version, min_minor_version, cmd, arg, re, expected) in test_cases {
            let re = Regex::new(re).unwrap();
            let actual = check_command(expected_major_version, min_minor_version, cmd, &[arg], &re).is_ok();
            assert_eq!(actual, expected);
        }
    }
}

fn analyze_video_file(path: impl AsRef<Path>) -> Option<InputFile> {
    let path = path.as_ref();
    let ffprobe::FfProbe { format, streams } = match ffprobe::ffprobe(&path) {
        Ok(ffprobe_info) => ffprobe_info,
        Err(err) => {
            log::warn!("Video file not support, ignored: {:} ({:})", path.display(), err);
            return None;
        },
    };

    analyze_video_file_impl(path, format, streams)
}

// separate impl for test
fn analyze_video_file_impl(path: &Path, format: ffprobe::Format, streams: Vec<ffprobe::Stream>) -> Option<InputFile> {
    let Some(video_stream) = get_first_video_stream(&streams) else {
        log::warn!("No video stream in file, ignored: {:}", path.display());
        return None;
    };

    let (Some(width), Some(height)) = (video_stream.width, video_stream.height) else {
        log::warn!("Couldn't get video resolution, ignored: {:}", path.display());
        return None;
    };

    if width < 0 || height < 0 {
        log::warn!("Invalid resolution, ignored: {:} ({:}, {:})", path.display(), width, height);
        return None;
    };

    
    let alternative_null_audio_duration = match get_first_audio_stream(&streams) {
        Some(_) => None,
        None => {
            let Some(video_duration) = get_stream_duration(&video_stream, &format) else {
                log::warn!("Couldn't get video duration, ignored: {:}", path.display());
                return None;
            };
            Some(video_duration)
        },
    };

    Some(InputFile { path: path.into(), width, height, alternative_null_audio_duration })
}

#[cfg(test)]
mod test_analyze_video_file {
    use super::*;
    use std::env;

    #[test]
    fn it_works() {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");

        let path = video_dir_path.join("va-300x400.mp4");
        assert!(analyze_video_file(&path).is_some());

        let ffprobe::FfProbe { mut format, streams } = ffprobe::ffprobe(&path).unwrap();

        let mut video_stream = get_first_video_stream(&streams).unwrap().clone();
        let audio_stream = get_first_audio_stream(&streams).unwrap().clone();

        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_some());
        assert!(analyze_video_file_impl(&path, format.clone(), vec![audio_stream.clone()]).is_none());

        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_some());
        video_stream.width = None;
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_none());
        video_stream.width = Some(300);

        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_some());
        video_stream.height = None;
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_none());
        video_stream.height = Some(400);

        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_some());
        video_stream.width = Some(-1);
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_none());
        video_stream.width = Some(400);

        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_some());
        video_stream.height = Some(-1);
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_none());
        video_stream.height = Some(400);

        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).is_some());
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone(), audio_stream.clone()]).unwrap().alternative_null_audio_duration.is_none());
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone()]).is_some());
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone()]).unwrap().alternative_null_audio_duration.is_some());

        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone()]).is_some());
        format.duration = None;
        video_stream.duration = None;
        assert!(analyze_video_file_impl(&path, format.clone(), vec![video_stream.clone()]).is_none());
    }
}

fn get_avfilter_code(input_files: &Vec<InputFile>) -> String {
    let mut filter_code = String::new();
    let mut concat_input_part_filter_code = String::new();

    assert!(0 < input_files.len());

    let target_width = input_files.iter().map(|input_file| { input_file.width }).max().expect("it must not be none, because input_files must not be 0");
    let target_height = input_files.iter().map(|input_file| { input_file.height }).max().expect("it must not be none, because input_files must not be 0");

    for (index, input_file) in input_files.iter().enumerate() {
        let part_video_filter_code = if input_file.width == target_width && input_file.height == target_height {
            "null".to_string()
        } else if input_file.width * target_height == input_file.height * target_width {
            // same aspect ratio
            format!("scale={:}:{:}", target_width, target_height)
        } else {
            format!("scale={0:}:{1:}:force_original_aspect_ratio=decrease,pad={0:}:{1:}:(ow-iw)/2:(oh-ih)/2", target_width, target_height)
        };
        let filter_code_statement = format!("[{0:}:v:0]{1:}[v{0:}];", index, part_video_filter_code);
        filter_code.push_str(&filter_code_statement);
        log::info!("Add filter: {:}", filter_code_statement);

        let filter_code_statement = if let Some(alternative_null_audio_duration) = input_file.alternative_null_audio_duration {
            format!("anullsrc=d={:}[a{:}];", alternative_null_audio_duration, index)
        } else {
            format!("[{0:}:a:0]anull[a{0:}];", index)
        };
        filter_code.push_str(&filter_code_statement);
        log::info!("Add filter: {:}", filter_code_statement);

        concat_input_part_filter_code.push_str(&format!("[v{0:}]", index));
        concat_input_part_filter_code.push_str(&format!("[a{0:}]", index));
    }

    let filter_code_statement = format!("{:}concat=n={:}:v=1:a=1[vout][aout]", concat_input_part_filter_code, input_files.len());

    log::info!("Add filter: {:}", filter_code_statement);
    filter_code.push_str(&filter_code_statement);
    filter_code
}

#[cfg(test)]
mod test_get_avfilter_code {
    use super::*;

    #[test]
    fn it_works() {
        let test_cases = [
            ("[0:v:0]null[v0];[0:a:0]anull[a0];[1:v:0]null[v1];[1:a:0]anull[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]", vec![
                InputFile { path: PathBuf::from("0.mp4"), width: 300, height: 100, alternative_null_audio_duration: None },
                InputFile { path: PathBuf::from("1.mp4"), width: 300, height: 100, alternative_null_audio_duration: None },
            ]),
            ("[0:v:0]null[v0];[0:a:0]anull[a0];[1:v:0]scale=300:100[v1];[1:a:0]anull[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]", vec![
                InputFile { path: PathBuf::from("0.mp4"), width: 300, height: 100, alternative_null_audio_duration: None },
                InputFile { path: PathBuf::from("1.mp4"), width: 150, height: 50, alternative_null_audio_duration: None },
            ]),
            ("[0:v:0]scale=300:150:force_original_aspect_ratio=decrease,pad=300:150:(ow-iw)/2:(oh-ih)/2[v0];[0:a:0]anull[a0];[1:v:0]scale=300:150:force_original_aspect_ratio=decrease,pad=300:150:(ow-iw)/2:(oh-ih)/2[v1];[1:a:0]anull[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]", vec![
                InputFile { path: PathBuf::from("0.mp4"), width: 300, height: 100, alternative_null_audio_duration: None },
                InputFile { path: PathBuf::from("1.mp4"), width: 50, height: 150, alternative_null_audio_duration: None },
            ]),
            ("[0:v:0]null[v0];anullsrc=d=3.5[a0];[1:v:0]null[v1];[1:a:0]anull[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]", vec![
                InputFile { path: PathBuf::from("0.mp4"), width: 300, height: 100, alternative_null_audio_duration: Some(3.5) },
                InputFile { path: PathBuf::from("1.mp4"), width: 300, height: 100, alternative_null_audio_duration: None },
            ]),
            ("[0:v:0]null[v0];[0:a:0]anull[a0];[1:v:0]null[v1];anullsrc=d=10.5[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[vout][aout]", vec![
                InputFile { path: PathBuf::from("0.mp4"), width: 300, height: 100, alternative_null_audio_duration: None },
                InputFile { path: PathBuf::from("1.mp4"), width: 300, height: 100, alternative_null_audio_duration: Some(10.5) },
            ]),
        ];

        for (filter, input_files) in test_cases {
            assert_eq!(get_avfilter_code(&input_files), filter.to_string());
        }
    }
}

fn get_stream_duration(stream: &ffprobe::Stream, format: &ffprobe::Format) -> Option<f64> {
    if let Some(duration) = &stream.duration {
        if let Ok(duration) = duration.parse::<f64>() {
            return Some(duration);
        }
    }

    if let Some(duration) = &format.duration {
        if let Ok(duration) = duration.parse::<f64>() {
            return Some(duration);
        }
    }

    None
}

#[cfg(test)]
mod test_get_stream_duration {
    use super::*;
    use std::env;

    #[test]
    fn it_works() {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");
        let ffprobe::FfProbe { mut format, streams } = ffprobe::ffprobe(video_dir_path.join("va-300x400.mp4")).unwrap();
        let stream = get_first_video_stream(&streams).unwrap();
        let mut stream = stream.clone();

        // stream=valid, format=valid
        assert!(get_stream_duration(&stream, &format).is_some());

        // stream=none, format=valid
        stream.duration = None;
        assert!(get_stream_duration(&stream, &format).is_some());

        // stream=none, format=none
        format.duration = None;
        assert!(get_stream_duration(&stream, &format).is_none());

        // stream=none, format=invalid
        format.duration = Some("invalid".to_string());
        assert!(get_stream_duration(&stream, &format).is_none());

        // stream=invalid, format=invalid
        stream.duration = Some("invalid".to_string());
        assert!(get_stream_duration(&stream, &format).is_none());

        // stream=valid, format=invalid
        stream.duration = Some("1.0".to_string());
        assert!(get_stream_duration(&stream, &format).is_some());
    }
}

fn get_first_stream_for_codec_type<'a>(codec_type: &str, streams: &'a Vec<ffprobe::Stream>) -> Option<&'a ffprobe::Stream> {
    for stream in streams {
        if stream.codec_type == Some(codec_type.to_string()) {
            return Some(stream);
        }
    }
    None
}

#[cfg(test)]
mod test_get_first_stream_for_codec_type {
    use super::*;
    use std::env;

    #[test]
    fn it_works() {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");

        let test_cases = [
            ("v-300x400.mp4", true, false),
            ("va-300x400.mp4", true, true),
            ("a.mp4", false, true),
        ];

        for (filename, expected_video, expected_audio) in test_cases {
            let ffprobe::FfProbe { streams, .. } = ffprobe::ffprobe(video_dir_path.join(filename)).unwrap();
            let actual_video = get_first_stream_for_codec_type("video", &streams).is_some();
            let actual_audio = get_first_stream_for_codec_type("audio", &streams).is_some();
            assert_eq!(actual_video, expected_video);
            assert_eq!(actual_audio, expected_audio);
        }
    }
}

fn get_first_video_stream<'a>(streams: &'a Vec<ffprobe::Stream>) -> Option<&'a ffprobe::Stream> {
    get_first_stream_for_codec_type("video", streams)
}

#[cfg(test)]
mod test_get_first_video_stream {
    use super::*;
    use std::env;

    #[test]
    fn it_works() {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");

        let test_cases = [
            ("v-300x400.mp4", true),
            ("va-300x400.mp4", true),
            ("a.mp4", false),
        ];

        for (filename, expected) in test_cases {
            let ffprobe::FfProbe { streams, .. } = ffprobe::ffprobe(video_dir_path.join(filename)).unwrap();
            let actual = get_first_video_stream(&streams).is_some();
            assert_eq!(actual, expected);
        }
    }
}

fn get_first_audio_stream<'a>(streams: &'a Vec<ffprobe::Stream>) -> Option<&'a ffprobe::Stream> {
    get_first_stream_for_codec_type("audio", streams)
}

#[cfg(test)]
mod test_get_first_audio_stream {
    use super::*;
    use std::env;

    #[test]
    fn it_works() {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");

        let test_cases = [
            ("v-300x400.mp4", false),
            ("va-300x400.mp4", true),
            ("a.mp4", true),
        ];

        for (filename, expected) in test_cases {
            let ffprobe::FfProbe { streams, .. } = ffprobe::ffprobe(video_dir_path.join(filename)).unwrap();
            let actual = get_first_audio_stream(&streams).is_some();
            assert_eq!(actual, expected);
        }
    }
}

fn get_best_crf(video_path: impl AsRef<Path>, enough_vmaf: u8, min_crf: u8) -> Result<(u8, Option<f64>), Error> {
    get_best_crf_impl(AB_AV1_CMD_STR, video_path, enough_vmaf, min_crf)
}

// separate impl for test
fn get_best_crf_impl(cmd_str: &str, video_path: impl AsRef<Path>, enough_vmaf: u8, min_crf: u8) -> Result<(u8, Option<f64>), Error> {
    let video_path = video_path.as_ref();

    let mut ab_av1_cmd = Command::new(cmd_str);
    ab_av1_cmd.args([
        "crf-search",
        "--min-vmaf", &enough_vmaf.to_string(),
        "--min-crf", &(min_crf + 1).to_string(),
        "--max-crf", &MAX_CRF.to_string(),
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
        assert!(caps.len() >= 2);
        let crf = parse_number::<u8, _>(&caps[1], Error { kind: ErrorKind::InvalidAbAv1Output(video_path.into(), stdout.clone()) })?;
        let vmaf = parse_number::<f64, _>(&caps[2], Error { kind: ErrorKind::InvalidAbAv1Output(video_path.into(), stdout.clone()) })?;
        Ok((crf, Some(vmaf)))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !AB_AV1_STDERR_CHECK_GOOD_CRF_NOT_FOUND_REGEX.is_match(&stderr) {
            return Err(Error { kind: ErrorKind::UnknownAbAv1ErrorMessage(video_path.into(), stderr) });
        }
        // if failed with not found good crf, then max crf
        Ok((min_crf, None))
    }
}

#[cfg(test)]
mod test_get_best_crf {
    use super::*;
    use std::env;

    #[test]
    fn it_works() {
        let root_path = env::var("CARGO_MANIFEST_DIR").unwrap();
        let root_path = Path::new(&root_path);
        let video_dir_path = root_path.join("tests/videos");

        assert!(match get_best_crf_impl("__command_not_found__", video_dir_path.join("va-300x400.mp4"), 80, 40) {
            Err(Error { kind: ErrorKind::AbAv1CommandProcessFailed(_, _) }) => true, _ => false,
        });
        assert!(match get_best_crf_impl("echo", video_dir_path.join("va-300x400.mp4"), 80, 40) {
            Err(Error { kind: ErrorKind::InvalidAbAv1Output(_, _) }) => true, _ => false,
        });
        assert!(match get_best_crf_impl("false", video_dir_path.join("va-300x400.mp4"), 80, 40) {
            Err(Error { kind: ErrorKind::UnknownAbAv1ErrorMessage(_, _) }) => true, _ => false,
        });
        assert_eq!(get_best_crf(video_dir_path.join("va-300x400.mp4"), 100, MAX_CRF - 2), Ok((MAX_CRF - 2, None)));
        assert!(match get_best_crf(video_dir_path.join("va-300x400.mp4"), 0, MAX_CRF - 2) {
            Ok((MAX_CRF, Some(_))) => true, _ => false,
        });
    }
}

// weird abstraction for test cov, the function contains else route so as to avoid uncoverable route in caller
fn parse_number<I: std::str::FromStr, Error>(s: &str, err: Error) -> Result<I, Error> {
    let Ok(u) = s.parse::<I>() else {
        return Err(err);
    };
    Ok(u)
}

#[cfg(test)]
mod test_parse_number {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(parse_number::<u8, _>("-1", "err"), Err("err"));
        assert_eq!(parse_number::<u8, _>("0", "err"), Ok(0));
        assert_eq!(parse_number::<u8, _>("255", "err"), Ok(255));
        assert_eq!(parse_number::<u8, _>("256", "err"), Err("err"));
        assert_eq!(parse_number::<u8, _>("a", "err"), Err("err"));
        assert_eq!(parse_number::<u8, _>("", "err"), Err("err"));
    }
}

