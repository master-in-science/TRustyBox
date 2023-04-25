use axum_extra::routing::RouterExt;
use dotenv::dotenv;

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query},
    http::{header::CONTENT_TYPE, HeaderName, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post, Router},
    Json,
};
use tower_http::cors::{Any, CorsLayer};

use encryption::{
    convert_aes_to_base64, convert_base64_to_aes, decrypt_data, encrypt_data, set_aes_key,
};

use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, env};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};

mod connection_to_db;
mod encryption;
mod tools;

const MAX_FILE_SIZE: usize = 15 * 1024 * 1024;

async fn download_file(Path(short_url): Path<String>) -> Result<impl IntoResponse, Infallible> {
    let (file_path_to_file, file_name) =
        match connection_to_db::get_name_and_path_of_file(short_url).await {
            Ok((file_path, file_name, is_encrypted)) => {
                if is_encrypted == true {
                    let response = Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body("Insert the AES key into URL".into())
                        .unwrap();
                    return Ok(response);
                }
                (file_path, file_name)
            }
            Err(_err) => {
                let response = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body("URL or FILE not found".into())
                    .unwrap();
                return Ok(response);
            }
        };

    match tokio::fs::File::open(&file_path_to_file).await {
        Ok(mut file) => {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).await.unwrap();

            let len = buf.len();

            let body = axum::body::Body::from(buf);
            let mut response = Response::new(body);

            let content_type = tools::check_content_type(&file_name).await;
            let content_disposition = format!("attachment; filename=\"{}\"", file_name);

            response
                .headers_mut()
                .insert(CONTENT_TYPE, content_type.parse().unwrap());
            response.headers_mut().insert(
                HeaderName::from_static("content-length"),
                HeaderValue::from_str(&len.to_string()).unwrap(),
            );
            response.headers_mut().insert(
                HeaderName::from_static("content-disposition"),
                HeaderValue::from_str(&content_disposition).unwrap(),
            );

            Ok(response)
        }
        Err(_) => {
            let response = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("FILE or URL not found".into())
                .unwrap();
            Ok(response)
        }
    }
}

async fn download_file_with_aes(
    Path((short_url, aes_key)): Path<(String, String)>,
) -> Result<impl IntoResponse, Infallible> {
    let (file_path_to_file, file_name) =
        match connection_to_db::get_name_and_path_of_file(short_url).await {
            Ok((file_path, file_name, _)) => (file_path, file_name),
            Err(_err) => {
                let response = Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body("URL or FILE not found".into())
                    .unwrap();
                return Ok(response);
            }
        };

    match tokio::fs::File::open(&file_path_to_file).await {
        Ok(mut file) => {
            let key_bytes = match convert_base64_to_aes(aes_key).await {
                Ok(key) => key,
                Err(_err) => {
                    let response = Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body("Invalid key".into())
                        .unwrap();
                    return Ok(response);
                }
            };

            let mut buf = Vec::new();
            file.read_to_end(&mut buf).await.unwrap();

            let data = decrypt_data(&buf, key_bytes).await.unwrap();

            let len = data.len();

            let body = axum::body::Body::from(data);
            let mut response = Response::new(body);

            let content_type = tools::check_content_type(&file_name).await;
            let content_disposition = format!("attachment; filename=\"{}\"", file_name);

            response
                .headers_mut()
                .insert(CONTENT_TYPE, content_type.parse().unwrap());
            response.headers_mut().insert(
                HeaderName::from_static("content-length"),
                HeaderValue::from_str(&len.to_string()).unwrap(),
            );
            response.headers_mut().insert(
                HeaderName::from_static("content-disposition"),
                HeaderValue::from_str(&content_disposition).unwrap(),
            );

            return Ok(response);
        }
        Err(_) => {
            let response = Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("FILE or URL not found".into())
                .unwrap();
            Ok(response)
        }
    }
}

async fn upload_file(
    Query(payload): Query<UploadPayload>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, Infallible> {
    while let Some(mut field) = multipart.next_field().await.unwrap() {
        let file_name = field.file_name().unwrap().to_owned();
        let new_filename = match file_name.split('.').last() {
            Some(extension) => format!("{}.{}", tools::generate_uuid_v4().await, extension),
            None => tools::generate_uuid_v4().await,
        };

        let generated_short_path = tools::generate_short_path_url().await;

        let file_path = format!(
            "{}{}",
            env::var("PATH_TO_FILES").expect("Unexpected error"),
            new_filename
        );

        let mut file = match File::create(&file_path).await {
            Ok(file) => file,
            Err(_err) => {
                let response = UploadResponse {
                    short_path: None,
                    full_url: None,
                    error: Some("Can't to upload file. Try again".to_string()),
                    aes_key: None,
                };
                return Ok((StatusCode::BAD_REQUEST, Json(response)));
            }
        };
        if let Some(header) = payload.encryption {
            if header == "aes".to_string() || header == "aes/".to_string() {
                let mut data = Vec::new();
                while let Some(chunk) = field.try_next().await.unwrap() {
                    data.extend_from_slice(&chunk);
                }

                let aes_key = set_aes_key().await;
                let encoded_key = convert_aes_to_base64(aes_key).await;

                let encrypted_data = match encrypt_data(&data, aes_key).await {
                    Ok(encrypted_data) => encrypted_data,
                    Err(_err) => {
                        let response = UploadResponse {
                            short_path: None,
                            full_url: None,
                            error: Some("Could not encrypt data. Try again.".to_string()),
                            aes_key: None,
                        };
                        return Ok((StatusCode::BAD_REQUEST, Json(response)));
                    }
                };
                file.write_all(&encrypted_data).await.unwrap();
                file.flush().await.unwrap();
                drop(file);
                match connection_to_db::insert_to_mongodb(
                    &file_path,
                    &new_filename,
                    &file_name,
                    generated_short_path.clone(),
                    true,
                )
                .await
                {
                    Ok(()) => (),
                    Err(_err) => {
                        let response = UploadResponse {
                            short_path: None,
                            full_url: None,
                            error: Some(
                                "Could not insert information to database. Try again".to_string(),
                            ),
                            aes_key: None,
                        };
                        return Ok((StatusCode::BAD_REQUEST, Json(response)));
                    }
                };

                let response = UploadResponse {
                    short_path: Some(generated_short_path.clone()),
                    full_url: Some(format!(
                        "http://{}/{}/{}",
                        env::var("SERVER_ADDR").expect("ADDR NOT FOUND"),
                        &generated_short_path,
                        encoded_key,
                    )),
                    error: None,
                    aes_key: Some(encoded_key),
                };
                return Ok((StatusCode::OK, Json(response)));
            }
        }

        while let Some(chunk) = field.try_next().await.unwrap() {
            file.write_all(&chunk).await.unwrap();
            file.flush().await.unwrap();
        }

        drop(file);
        match connection_to_db::insert_to_mongodb(
            &file_path,
            &new_filename,
            &file_name,
            generated_short_path.clone(),
            false,
        )
        .await
        {
            Ok(()) => (),
            Err(_err) => {
                let response = UploadResponse {
                    short_path: None,
                    full_url: None,
                    error: Some("Could not insert information to database. Try again".to_string()),
                    aes_key: None,
                };
                return Ok((StatusCode::BAD_REQUEST, Json(response)));
            }
        };

        let response = UploadResponse {
            short_path: Some(generated_short_path.clone()),
            full_url: Some(format!(
                "http://{}/{}",
                env::var("SERVER_ADDR").expect("ADDR NOT FOUND"),
                &generated_short_path,
            )),
            error: None,
            aes_key: None,
        };
        return Ok((StatusCode::OK, Json(response)));
    }
    let response = UploadResponse {
        short_path: None,
        full_url: None,
        error: Some("FILE to download NOT FOUND".to_string()),
        aes_key: None,
    };
    return Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(response)));
}

#[derive(Deserialize)]
struct UploadPayload {
    encryption: Option<String>,
}

#[derive(Serialize)]
struct UploadResponse {
    short_path: Option<String>,
    error: Option<String>,
    full_url: Option<String>,
    aes_key: Option<String>,
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    let cors = CorsLayer::new().allow_origin(Any);

    let app = Router::new()
        .route("/", post(upload_file))
        .layer(DefaultBodyLimit::max(MAX_FILE_SIZE))
        .layer(cors)
        .route_with_tsr("/:path", get(download_file))
        .route_with_tsr("/:path/:aes_key", get(download_file_with_aes));

    let addr = env::var("SERVER_ADDR")
        .expect("ADDR NOT FOUND")
        .parse()
        .unwrap();
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}