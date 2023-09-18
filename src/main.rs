use std::{
    env,
    path::Path,
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

#[tokio::main]
async fn main() {
    let bucket = match env::var("BUCKET") {
        Ok(bucket) => bucket, 
        Err(err) => panic!("BUCKET env var not set or invalid utf-8: {:}", err),
    };
    let object_ids = env::args().skip(1).collect();

    let config = ClientConfig::default().with_auth().await.expect("Couldn't auth");

    download_objects(config, bucket, object_ids).await;
}

async fn download_objects(config: ClientConfig, bucket: String, object_ids: Vec<String>) {
    let client = Client::new(config);
    for object_id in object_ids.into_iter() {
        download_object(&client, bucket.clone(), object_id).await;
    }
}

async fn download_object(client: &Client, bucket: String, object_id: String) {
    let Ok(mut object_stream) = client.download_streamed_object(&GetObjectRequest {
        bucket: bucket,
        object: object_id.clone(),
        ..Default::default()
    }, &Range::default()).await else {
        panic!("Couldn't get object stream: {:}", object_id);
    };

    let save_path = Path::new("data").join(&object_id);
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

