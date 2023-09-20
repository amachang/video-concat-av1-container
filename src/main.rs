mod video;

use std::{
    env,
    path::{
        Path,
        PathBuf,
    },
};
use google_cloud_storage::{
    client::{
        Client,
        ClientConfig,
    },
    http::objects::{
        download::Range,
        upload::{
            Media,
            UploadObjectRequest,
            UploadType,
        },
        get::GetObjectRequest,
    },
};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
};
use tokio_util::io::ReaderStream;
use futures::stream::StreamExt;

#[tokio::main]
async fn main() {
    let input_bucket = get_env_string("INPUT_BUCKET");
    let output_bucket = get_env_string("OUTPUT_BUCKET");
    let enough_vmaf = match get_env_string("ENOUGH_VMAF").parse::<u8>() {
        Ok(enough_vmaf) => enough_vmaf,
        Err(err) => panic!("ENOUGH_VMAF couldn't parse as unsigned int: {:}", err),
    };
    let min_crf = match get_env_string("MIN_CRF").parse::<u8>() {
        Ok(min_crf) => min_crf,
        Err(err) => panic!("MIN_CRF couldn't parse as unsigned int: {:}", err),
    };

    let mut args = env::args().skip(1);

    let Some(output_object_id) = args.next() else {
        panic!("No output gcs object id given");
    };
    let output_object_path = Path::new("output").join(&output_object_id);

    let object_ids = args.collect::<Vec<_>>();
    if object_ids.len() == 0 {
        panic!("No gcs object id given");
    }

    let config = ClientConfig::default().with_auth().await.expect("Couldn't auth");
    let client = Client::new(config);

    let object_paths = download_objects(&client, input_bucket, object_ids).await;

    video::encode_best_effort(object_paths, &output_object_path, enough_vmaf, min_crf);

    upload_object(&client, output_bucket, output_object_id, output_object_path).await
}

async fn download_objects(client: &Client, bucket: String, object_ids: Vec<String>) -> Vec<PathBuf> {
    let mut object_paths = Vec::new();
    for object_id in object_ids.into_iter() {
        let object_path = Path::new("data").join(&object_id);
        download_object(&client, bucket.clone(), object_id, &object_path).await;
        object_paths.push(object_path);
    }
    object_paths
}

async fn download_object(client: &Client, bucket: String, object_id: String, path: impl AsRef<Path>) {
    let Ok(mut object_stream) = client.download_streamed_object(&GetObjectRequest {
        bucket, object: object_id.clone(),
        ..Default::default()
    }, &Range::default()).await else {
        panic!("Couldn't get object stream: {:}", object_id);
    };

    let path = path.as_ref();
    let Ok(mut file) = File::create(path.clone()).await else {
        panic!("Couldn't create the path: {:}", path.display());
    };

    while let Some(item) = object_stream.next().await {
        let Ok(bytes) = item else {
            panic!("Couldn't receive bytes in object: {:}", object_id);
        };
        if let Err(err) = file.write_all(&bytes).await {
            panic!("Couldn't write bytes to file: {:} ({:})", path.display(), err);
        };
    }
}

async fn upload_object(client: &Client, bucket: String, object_id: String, path: impl AsRef<Path>) {
    let path = path.as_ref();
    
    let Ok(file) = File::open(path.clone()).await else {
        panic!("Couldn't open the path: {:}", path.display());
    };

    let Ok(metadata) = file.metadata().await else {
        panic!("Couldn't get a file metadata: {:}", path.display());
    };

    if !metadata.is_file() {
        panic!("Upload target not a file: {:}", path.display());
    };

    let mut media = Media::new(object_id);
    media.content_length = Some(metadata.len());

    let stream = ReaderStream::new(file);

    let upload_type = UploadType::Simple(media);
    if let Err(err) = client.upload_streamed_object(&UploadObjectRequest { bucket, ..Default::default() }, stream, &upload_type).await {
        panic!("Upload failed with error: {:} {:}", path.display(), err);
    };
}

fn get_env_string(name: &str) -> String {
    match env::var(name) {
        Ok(bucket) => bucket, 
        Err(err) => panic!("{:} env var not set or invalid utf-8: {:}", name, err),
    }
}
