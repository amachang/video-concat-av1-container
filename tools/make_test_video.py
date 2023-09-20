import os
import sys
import subprocess
import argparse
import tempfile
import wave
import numpy as np
import cv2

def create_random_video(duration, width, height, video_path):
    fps = 30
    fourcc = cv2.VideoWriter_fourcc(*'XVID')
    out = cv2.VideoWriter(video_path, fourcc, fps, (width, height))
    for _ in range(fps * duration):
        random_image = np.random.randint(0, 256, (height, width, 3), dtype=np.uint8)
        out.write(random_image)
    out.release()

def create_random_audio(duration, audio_path):
    sample_rate = 44100
    num_samples = sample_rate * duration
    samples = np.random.randint(-32768, 32767, num_samples, dtype=np.int16)

    with wave.open(audio_path, 'w') as w:
        w.setnchannels(1)
        w.setsampwidth(2)  # 16-bit
        w.setframerate(sample_rate)
        w.writeframes(samples.tobytes())

def main():
    parser = argparse.ArgumentParser(description="Generate random media files.")
    parser.add_argument("-v", "--video", action="store_true", help="Include video stream.")
    parser.add_argument("-a", "--audio", action="store_true", help="Include audio stream.")
    parser.add_argument("-d", "--duration", required=True, type=int, help="Duration in seconds.")
    parser.add_argument("-vw", "--width", type=int, help="Video width.")
    parser.add_argument("-vh", "--height", type=int, help="Video height.")
    parser.add_argument("-o", "--output", required=True, type=str, help="Output file path.")
    args = parser.parse_args()

    if not args.video and not args.audio:
        print("--video or --audio flags needed", file=sys.stderr)
        sys.exit(1);
    if args.video and (args.width is None or args.height is None):
        print("if set --video then --width and --height options needed", file=sys.stderr)
        sys.exit(1);

    with tempfile.NamedTemporaryFile(suffix=".avi", delete=False) as video_temp:
        video_path = video_temp.name
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as audio_temp:
        audio_path = audio_temp.name

    if args.video and args.audio:
        create_random_video(args.duration, args.width, args.height, video_path)
        create_random_audio(args.duration, audio_path)
        cmd = ["ffmpeg", "-i", video_path, "-i", audio_path, "-c:v", "copy", "-c:a", "aac", args.output]
        subprocess.run(cmd)
    elif args.video:
        create_random_video(args.duration, args.width, args.height, video_path)
        cmd = ["ffmpeg", "-i", video_path, "-c:v", "copy", "-an", args.output]
        subprocess.run(cmd)
    elif args.audio:
        create_random_audio(args.duration, audio_path)
        cmd = ["ffmpeg", "-i", audio_path, "-c:a", "aac", "-vn", args.output]
        subprocess.run(cmd)
    else:
        assert False

    if os.path.exists(video_path):
        os.remove(video_path)
    if os.path.exists(audio_path):
        os.remove(audio_path)

if __name__ == "__main__":
    main()

