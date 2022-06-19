use crate::auth::generate_www_auth;
use crate::streamer::Streamer;
use crate::utils::{decode_uri, encode_uri};
use crate::{Args, BoxResult};
use xml::escape::escape_str_pcdata;

use async_walkdir::WalkDir;
use async_zip::write::{EntryOptions, ZipFileWriter};
use async_zip::Compression;
use chrono::{TimeZone, Utc};
use futures::stream::StreamExt;
use futures::TryStreamExt;
use headers::{
    AcceptRanges, AccessControlAllowCredentials, AccessControlAllowHeaders,
    AccessControlAllowOrigin, Connection, ContentLength, ContentType, ETag, HeaderMap,
    HeaderMapExt, IfModifiedSince, IfNoneMatch, IfRange, LastModified, Range,
};
use hyper::header::{
    HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_TYPE, ORIGIN, RANGE, WWW_AUTHENTICATE,
};
use hyper::{Body, Method, StatusCode, Uri};
use serde::Serialize;
use std::fs::Metadata;
use std::io::SeekFrom;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWrite};
use tokio::{fs, io};
use tokio_util::io::StreamReader;
use uuid::Uuid;

pub type Request = hyper::Request<Body>;
pub type Response = hyper::Response<Body>;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const INDEX_CSS: &str = include_str!("../assets/index.css");
const INDEX_JS: &str = include_str!("../assets/index.js");
const FAVICON_ICO: &[u8] = include_bytes!("../assets/favicon.ico");
const INDEX_NAME: &str = "index.html";
const BUF_SIZE: usize = 65536;

pub struct Server {
    args: Arc<Args>,
}

impl Server {
    pub fn new(args: Arc<Args>) -> Self {
        Self { args }
    }

    pub async fn call(
        self: Arc<Self>,
        req: Request,
        addr: SocketAddr,
    ) -> Result<Response, hyper::Error> {
        let method = req.method().clone();
        let uri = req.uri().clone();
        let enable_cors = self.args.enable_cors;

        let mut res = match self.handle(req).await {
            Ok(res) => {
                let status = res.status().as_u16();
                info!(r#"{} "{} {}" - {}"#, addr.ip(), method, uri, status,);
                res
            }
            Err(err) => {
                let mut res = Response::default();
                let status = StatusCode::INTERNAL_SERVER_ERROR;
                *res.status_mut() = status;
                let status = status.as_u16();
                error!(r#"{} "{} {}" - {} {}"#, addr.ip(), method, uri, status, err);
                res
            }
        };

        if enable_cors {
            add_cors(&mut res);
        }
        Ok(res)
    }

    pub async fn handle(self: Arc<Self>, req: Request) -> BoxResult<Response> {
        let mut res = Response::default();

        let req_path = req.uri().path();
        let headers = req.headers();
        let method = req.method().clone();

        if req_path == "/favicon.ico" && method == Method::GET {
            self.handle_send_favicon(headers, &mut res).await?;
            return Ok(res);
        }

        let authorization = headers.get(AUTHORIZATION);
        let guard_type = self.args.auth.guard(req_path, &method, authorization);
        if guard_type.is_reject() {
            self.auth_reject(&mut res);
            return Ok(res);
        }

        let head_only = method == Method::HEAD;

        if self.args.path_is_file {
            self.handle_send_file(&self.args.path, headers, head_only, &mut res)
                .await?;
            return Ok(res);
        }

        let path = match self.extract_path(req_path) {
            Some(v) => v,
            None => {
                status_forbid(&mut res);
                return Ok(res);
            }
        };

        let path = path.as_path();

        let query = req.uri().query().unwrap_or_default();

        let (is_miss, is_dir, is_file, size) = match fs::metadata(path).await.ok() {
            Some(meta) => (false, meta.is_dir(), meta.is_file(), meta.len()),
            None => (true, false, false, 0),
        };

        let allow_upload = self.args.allow_upload;
        let allow_delete = self.args.allow_delete;
        let render_index = self.args.render_index;
        let render_spa = self.args.render_spa;
        let render_try_index = self.args.render_try_index;

        if !self.args.allow_symlink && !is_miss && !self.is_root_contained(path).await {
            status_not_found(&mut res);
            return Ok(res);
        }

        match method {
            Method::GET | Method::HEAD => {
                if is_dir {
                    if render_try_index && query == "zip" {
                        self.handle_zip_dir(path, head_only, &mut res).await?;
                    } else if render_index || render_spa || render_try_index {
                        self.handle_render_index(path, headers, head_only, &mut res)
                            .await?;
                    } else if query == "zip" {
                        self.handle_zip_dir(path, head_only, &mut res).await?;
                    } else if let Some(q) = query.strip_prefix("q=") {
                        self.handle_query_dir(path, q, head_only, &mut res).await?;
                    } else {
                        self.handle_ls_dir(path, true, head_only, &mut res).await?;
                    }
                } else if is_file {
                    self.handle_send_file(path, headers, head_only, &mut res)
                        .await?;
                } else if render_spa {
                    self.handle_render_spa(path, headers, head_only, &mut res)
                        .await?;
                } else if allow_upload && req_path.ends_with('/') {
                    self.handle_ls_dir(path, false, head_only, &mut res).await?;
                } else {
                    status_not_found(&mut res);
                }
            }
            Method::OPTIONS => {
                set_webdav_headers(&mut res);
            }
            Method::PUT => {
                if !allow_upload || (!allow_delete && is_file && size > 0) {
                    status_forbid(&mut res);
                } else {
                    self.handle_upload(path, req, &mut res).await?;
                }
            }
            Method::DELETE => {
                if !allow_delete {
                    status_forbid(&mut res);
                } else if !is_miss {
                    self.handle_delete(path, is_dir, &mut res).await?
                } else {
                    status_not_found(&mut res);
                }
            }
            method => match method.as_str() {
                "PROPFIND" => {
                    if is_dir {
                        self.handle_propfind_dir(path, headers, &mut res).await?;
                    } else if is_file {
                        self.handle_propfind_file(path, &mut res).await?;
                    } else {
                        status_not_found(&mut res);
                    }
                }
                "PROPPATCH" => {
                    if is_file {
                        self.handle_proppatch(req_path, &mut res).await?;
                    } else {
                        status_not_found(&mut res);
                    }
                }
                "MKCOL" => {
                    if !allow_upload || !is_miss {
                        status_forbid(&mut res);
                    } else {
                        self.handle_mkcol(path, &mut res).await?;
                    }
                }
                "COPY" => {
                    if !allow_upload {
                        status_forbid(&mut res);
                    } else if is_miss {
                        status_not_found(&mut res);
                    } else {
                        self.handle_copy(path, headers, &mut res).await?
                    }
                }
                "MOVE" => {
                    if !allow_upload || !allow_delete {
                        status_forbid(&mut res);
                    } else if is_miss {
                        status_not_found(&mut res);
                    } else {
                        self.handle_move(path, headers, &mut res).await?
                    }
                }
                "LOCK" => {
                    // Fake lock
                    if is_file {
                        let has_auth = authorization.is_some();
                        self.handle_lock(req_path, has_auth, &mut res).await?;
                    } else {
                        status_not_found(&mut res);
                    }
                }
                "UNLOCK" => {
                    // Fake unlock
                    if is_miss {
                        status_not_found(&mut res);
                    }
                }
                _ => {
                    *res.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
                }
            },
        }
        Ok(res)
    }

    async fn handle_upload(
        &self,
        path: &Path,
        mut req: Request,
        res: &mut Response,
    ) -> BoxResult<()> {
        ensure_path_parent(path).await?;

        let mut file = match fs::File::create(&path).await {
            Ok(v) => v,
            Err(_) => {
                status_forbid(res);
                return Ok(());
            }
        };

        let body_with_io_error = req
            .body_mut()
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err));

        let body_reader = StreamReader::new(body_with_io_error);

        futures::pin_mut!(body_reader);

        io::copy(&mut body_reader, &mut file).await?;

        *res.status_mut() = StatusCode::CREATED;
        Ok(())
    }

    async fn handle_delete(&self, path: &Path, is_dir: bool, res: &mut Response) -> BoxResult<()> {
        match is_dir {
            true => fs::remove_dir_all(path).await?,
            false => fs::remove_file(path).await?,
        }

        status_no_content(res);
        Ok(())
    }

    async fn handle_ls_dir(
        &self,
        path: &Path,
        exist: bool,
        head_only: bool,
        res: &mut Response,
    ) -> BoxResult<()> {
        let mut paths = vec![];
        if exist {
            paths = match self.list_dir(path, path).await {
                Ok(paths) => paths,
                Err(_) => {
                    status_forbid(res);
                    return Ok(());
                }
            }
        };
        self.send_index(path, paths, exist, head_only, res)
    }

    async fn handle_query_dir(
        &self,
        path: &Path,
        query: &str,
        head_only: bool,
        res: &mut Response,
    ) -> BoxResult<()> {
        let mut paths: Vec<PathItem> = vec![];
        let mut walkdir = WalkDir::new(path);
        while let Some(entry) = walkdir.next().await {
            if let Ok(entry) = entry {
                if !entry
                    .file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query.to_lowercase())
                {
                    continue;
                }
                if fs::symlink_metadata(entry.path()).await.is_err() {
                    continue;
                }
                if let Ok(Some(item)) = self.to_pathitem(entry.path(), path.to_path_buf()).await {
                    paths.push(item);
                }
            }
        }
        self.send_index(path, paths, true, head_only, res)
    }

    async fn handle_zip_dir(
        &self,
        path: &Path,
        head_only: bool,
        res: &mut Response,
    ) -> BoxResult<()> {
        let (mut writer, reader) = tokio::io::duplex(BUF_SIZE);
        let filename = get_file_name(path)?;
        res.headers_mut().insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!(
                "attachment; filename=\"{}.zip\"",
                encode_uri(filename),
            ))
            .unwrap(),
        );
        res.headers_mut()
            .insert("content-type", HeaderValue::from_static("application/zip"));
        if head_only {
            return Ok(());
        }
        let path = path.to_owned();
        tokio::spawn(async move {
            if let Err(e) = zip_dir(&mut writer, &path).await {
                error!("Failed to zip {}, {}", path.display(), e);
            }
        });
        let reader = Streamer::new(reader, BUF_SIZE);
        *res.body_mut() = Body::wrap_stream(reader.into_stream());
        Ok(())
    }

    async fn handle_render_index(
        &self,
        path: &Path,
        headers: &HeaderMap<HeaderValue>,
        head_only: bool,
        res: &mut Response,
    ) -> BoxResult<()> {
        let index_path = path.join(INDEX_NAME);
        if fs::metadata(&index_path)
            .await
            .ok()
            .map(|v| v.is_file())
            .unwrap_or_default()
        {
            self.handle_send_file(&index_path, headers, head_only, res)
                .await?;
        } else if self.args.render_try_index {
            self.handle_ls_dir(path, true, head_only, res).await?;
        } else {
            status_not_found(res)
        }
        Ok(())
    }

    async fn handle_render_spa(
        &self,
        path: &Path,
        headers: &HeaderMap<HeaderValue>,
        head_only: bool,
        res: &mut Response,
    ) -> BoxResult<()> {
        if path.extension().is_none() {
            let path = self.args.path.join(INDEX_NAME);
            self.handle_send_file(&path, headers, head_only, res)
                .await?;
        } else {
            status_not_found(res)
        }
        Ok(())
    }

    async fn handle_send_favicon(
        &self,
        headers: &HeaderMap<HeaderValue>,
        res: &mut Response,
    ) -> BoxResult<()> {
        let path = self.args.path.join("favicon.ico");
        let meta = fs::metadata(&path).await.ok();
        let is_file = meta.map(|v| v.is_file()).unwrap_or_default();
        if is_file {
            self.handle_send_file(path.as_path(), headers, false, res)
                .await?;
        } else {
            *res.body_mut() = Body::from(FAVICON_ICO);
            res.headers_mut()
                .insert("content-type", HeaderValue::from_static("image/x-icon"));
        }
        Ok(())
    }

    async fn handle_send_file(
        &self,
        path: &Path,
        headers: &HeaderMap<HeaderValue>,
        head_only: bool,
        res: &mut Response,
    ) -> BoxResult<()> {
        let (file, meta) = tokio::join!(fs::File::open(path), fs::metadata(path),);
        let (mut file, meta) = (file?, meta?);
        let mut use_range = true;
        if let Some((etag, last_modified)) = extract_cache_headers(&meta) {
            let cached = {
                if let Some(if_none_match) = headers.typed_get::<IfNoneMatch>() {
                    !if_none_match.precondition_passes(&etag)
                } else if let Some(if_modified_since) = headers.typed_get::<IfModifiedSince>() {
                    !if_modified_since.is_modified(last_modified.into())
                } else {
                    false
                }
            };
            if cached {
                *res.status_mut() = StatusCode::NOT_MODIFIED;
                return Ok(());
            }

            res.headers_mut().typed_insert(last_modified);
            res.headers_mut().typed_insert(etag.clone());

            if headers.typed_get::<Range>().is_some() {
                use_range = headers
                    .typed_get::<IfRange>()
                    .map(|if_range| !if_range.is_modified(Some(&etag), Some(&last_modified)))
                    // Always be fresh if there is no validators
                    .unwrap_or(true);
            } else {
                use_range = false;
            }
        }

        let range = if use_range {
            parse_range(headers)
        } else {
            None
        };

        if let Some(mime) = mime_guess::from_path(&path).first() {
            res.headers_mut().typed_insert(ContentType::from(mime));
        } else {
            res.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
        }

        let filename = get_file_name(path)?;
        res.headers_mut().insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!("inline; filename=\"{}\"", encode_uri(filename),))
                .unwrap(),
        );

        res.headers_mut().typed_insert(AcceptRanges::bytes());

        let size = meta.len();

        if let Some(range) = range {
            if range
                .end
                .map_or_else(|| range.start < size, |v| v >= range.start)
                && file.seek(SeekFrom::Start(range.start)).await.is_ok()
            {
                let end = range.end.unwrap_or(size - 1).min(size - 1);
                let part_size = end - range.start + 1;
                let reader = Streamer::new(file, BUF_SIZE);
                *res.status_mut() = StatusCode::PARTIAL_CONTENT;
                let content_range = format!("bytes {}-{}/{}", range.start, end, size);
                res.headers_mut()
                    .insert(CONTENT_RANGE, content_range.parse().unwrap());
                res.headers_mut()
                    .insert(CONTENT_LENGTH, format!("{}", part_size).parse().unwrap());
                if head_only {
                    return Ok(());
                }
                *res.body_mut() = Body::wrap_stream(reader.into_stream_sized(part_size));
            } else {
                *res.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                res.headers_mut()
                    .insert(CONTENT_RANGE, format!("bytes */{}", size).parse().unwrap());
            }
        } else {
            res.headers_mut()
                .insert(CONTENT_LENGTH, format!("{}", size).parse().unwrap());
            if head_only {
                return Ok(());
            }
            let reader = Streamer::new(file, BUF_SIZE);
            *res.body_mut() = Body::wrap_stream(reader.into_stream());
        }
        Ok(())
    }

    async fn handle_propfind_dir(
        &self,
        path: &Path,
        headers: &HeaderMap<HeaderValue>,
        res: &mut Response,
    ) -> BoxResult<()> {
        let depth: u32 = match headers.get("depth") {
            Some(v) => match v.to_str().ok().and_then(|v| v.parse().ok()) {
                Some(v) => v,
                None => {
                    *res.status_mut() = StatusCode::BAD_REQUEST;
                    return Ok(());
                }
            },
            None => 1,
        };
        let mut paths = vec![self.to_pathitem(path, &self.args.path).await?.unwrap()];
        if depth != 0 {
            match self.list_dir(path, &self.args.path).await {
                Ok(child) => paths.extend(child),
                Err(_) => {
                    status_forbid(res);
                    return Ok(());
                }
            }
        }
        let output = paths
            .iter()
            .map(|v| v.to_dav_xml(self.args.uri_prefix.as_str()))
            .fold(String::new(), |mut acc, v| {
                acc.push_str(&v);
                acc
            });
        res_multistatus(res, &output);
        Ok(())
    }

    async fn handle_propfind_file(&self, path: &Path, res: &mut Response) -> BoxResult<()> {
        if let Some(pathitem) = self.to_pathitem(path, &self.args.path).await? {
            res_multistatus(res, &pathitem.to_dav_xml(self.args.uri_prefix.as_str()));
        } else {
            status_not_found(res);
        }
        Ok(())
    }

    async fn handle_mkcol(&self, path: &Path, res: &mut Response) -> BoxResult<()> {
        fs::create_dir_all(path).await?;
        *res.status_mut() = StatusCode::CREATED;
        Ok(())
    }

    async fn handle_copy(
        &self,
        path: &Path,
        headers: &HeaderMap<HeaderValue>,
        res: &mut Response,
    ) -> BoxResult<()> {
        let dest = match self.extract_dest(headers) {
            Some(dest) => dest,
            None => {
                *res.status_mut() = StatusCode::BAD_REQUEST;
                return Ok(());
            }
        };

        let meta = fs::symlink_metadata(path).await?;
        if meta.is_dir() {
            status_forbid(res);
            return Ok(());
        }

        ensure_path_parent(&dest).await?;

        fs::copy(path, &dest).await?;

        status_no_content(res);
        Ok(())
    }

    async fn handle_move(
        &self,
        path: &Path,
        headers: &HeaderMap<HeaderValue>,
        res: &mut Response,
    ) -> BoxResult<()> {
        let dest = match self.extract_dest(headers) {
            Some(dest) => dest,
            None => {
                *res.status_mut() = StatusCode::BAD_REQUEST;
                return Ok(());
            }
        };

        ensure_path_parent(&dest).await?;

        fs::rename(path, &dest).await?;

        status_no_content(res);
        Ok(())
    }

    async fn handle_lock(&self, req_path: &str, auth: bool, res: &mut Response) -> BoxResult<()> {
        let token = if auth {
            format!("opaquelocktoken:{}", Uuid::new_v4())
        } else {
            Utc::now().timestamp().to_string()
        };

        res.headers_mut().insert(
            "content-type",
            HeaderValue::from_static("application/xml; charset=utf-8"),
        );
        res.headers_mut()
            .insert("lock-token", format!("<{}>", token).parse().unwrap());

        *res.body_mut() = Body::from(format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:prop xmlns:D="DAV:"><D:lockdiscovery><D:activelock>
<D:locktoken><D:href>{}</D:href></D:locktoken>
<D:lockroot><D:href>{}</D:href></D:lockroot>
</D:activelock></D:lockdiscovery></D:prop>"#,
            token, req_path
        ));
        Ok(())
    }

    async fn handle_proppatch(&self, req_path: &str, res: &mut Response) -> BoxResult<()> {
        let output = format!(
            r#"<D:response>
<D:href>{}</D:href>
<D:propstat>
<D:prop>
</D:prop>
<D:status>HTTP/1.1 403 Forbidden</D:status>
</D:propstat>
</D:response>"#,
            req_path
        );
        res_multistatus(res, &output);
        Ok(())
    }

    fn send_index(
        &self,
        path: &Path,
        mut paths: Vec<PathItem>,
        exist: bool,
        head_only: bool,
        res: &mut Response,
    ) -> BoxResult<()> {
        paths.sort_unstable();
        let breadcrumb = format!("/{}", normalize_path(path.strip_prefix(&self.args.path)?));
        let data = IndexData {
            breadcrumb: breadcrumb.clone(),
            paths,
            allow_upload: self.args.allow_upload,
            allow_delete: self.args.allow_delete,
            dir_exists: exist,
        };
        let data = serde_json::to_string(&data).unwrap();
        let output = INDEX_HTML.replace(
            "__SLOT__",
            &format!(
                r#"
<title>Index of {} - Duf</title>
<style>{}</style>
<script>
const DATA = 
{}
{}</script>
"#,
                breadcrumb, INDEX_CSS, data, INDEX_JS
            ),
        );
        res.headers_mut()
            .typed_insert(ContentType::from(mime_guess::mime::TEXT_HTML_UTF_8));
        res.headers_mut()
            .typed_insert(ContentLength(output.as_bytes().len() as u64));
        if head_only {
            return Ok(());
        }
        *res.body_mut() = output.into();
        Ok(())
    }

    fn auth_reject(&self, res: &mut Response) {
        let value = generate_www_auth(false);
        set_webdav_headers(res);
        res.headers_mut().typed_insert(Connection::close());
        res.headers_mut()
            .insert(WWW_AUTHENTICATE, value.parse().unwrap());
        *res.status_mut() = StatusCode::UNAUTHORIZED;
    }

    async fn is_root_contained(&self, path: &Path) -> bool {
        fs::canonicalize(path)
            .await
            .ok()
            .map(|v| v.starts_with(&self.args.path))
            .unwrap_or_default()
    }

    fn extract_dest(&self, headers: &HeaderMap<HeaderValue>) -> Option<PathBuf> {
        let dest = headers.get("Destination")?.to_str().ok()?;
        let uri: Uri = dest.parse().ok()?;
        self.extract_path(uri.path())
    }

    fn extract_path(&self, path: &str) -> Option<PathBuf> {
        let decoded_path = decode_uri(&path[1..])?;
        let slashes_switched = if cfg!(windows) {
            decoded_path.replace('/', "\\")
        } else {
            decoded_path.into_owned()
        };
        let stripped_path = match self.strip_path_prefix(&slashes_switched) {
            Some(path) => path,
            None => return None,
        };
        Some(self.args.path.join(&stripped_path))
    }

    fn strip_path_prefix<'a, P: AsRef<Path>>(&self, path: &'a P) -> Option<&'a Path> {
        let path = path.as_ref();
        if self.args.path_prefix.is_empty() {
            Some(path)
        } else {
            path.strip_prefix(&self.args.path_prefix).ok()
        }
    }

    async fn list_dir(&self, entry_path: &Path, base_path: &Path) -> BoxResult<Vec<PathItem>> {
        let mut paths: Vec<PathItem> = vec![];
        let mut rd = fs::read_dir(entry_path).await?;
        while let Ok(Some(entry)) = rd.next_entry().await {
            let entry_path = entry.path();
            if let Ok(Some(item)) = self.to_pathitem(entry_path.as_path(), base_path).await {
                paths.push(item);
            }
        }
        Ok(paths)
    }

    async fn to_pathitem<P: AsRef<Path>>(
        &self,
        path: P,
        base_path: P,
    ) -> BoxResult<Option<PathItem>> {
        let path = path.as_ref();
        let rel_path = path.strip_prefix(base_path).unwrap();
        let (meta, meta2) = tokio::join!(fs::metadata(&path), fs::symlink_metadata(&path));
        let (meta, meta2) = (meta?, meta2?);
        let is_symlink = meta2.is_symlink();
        if !self.args.allow_symlink && is_symlink && !self.is_root_contained(path).await {
            return Ok(None);
        }
        let is_dir = meta.is_dir();
        let path_type = match (is_symlink, is_dir) {
            (true, true) => PathType::SymlinkDir,
            (false, true) => PathType::Dir,
            (true, false) => PathType::SymlinkFile,
            (false, false) => PathType::File,
        };
        let mtime = to_timestamp(&meta.modified()?);
        let size = match path_type {
            PathType::Dir | PathType::SymlinkDir => None,
            PathType::File | PathType::SymlinkFile => Some(meta.len()),
        };
        let name = normalize_path(rel_path);
        Ok(Some(PathItem {
            path_type,
            name,
            mtime,
            size,
        }))
    }
}

#[derive(Debug, Serialize)]
struct IndexData {
    breadcrumb: String,
    paths: Vec<PathItem>,
    allow_upload: bool,
    allow_delete: bool,
    dir_exists: bool,
}

#[derive(Debug, Serialize, Eq, PartialEq, Ord, PartialOrd)]
struct PathItem {
    path_type: PathType,
    name: String,
    mtime: u64,
    size: Option<u64>,
}

impl PathItem {
    pub fn is_dir(&self) -> bool {
        self.path_type == PathType::Dir || self.path_type == PathType::SymlinkDir
    }

    pub fn to_dav_xml(&self, prefix: &str) -> String {
        let mtime = Utc.timestamp_millis(self.mtime as i64).to_rfc2822();
        let mut href = encode_uri(&format!("{}{}", prefix, &self.name));
        if self.is_dir() && !href.ends_with('/') {
            href.push('/');
        }
        let displayname = escape_str_pcdata(self.base_name());
        match self.path_type {
            PathType::Dir | PathType::SymlinkDir => format!(
                r#"<D:response>
<D:href>{}</D:href>
<D:propstat>
<D:prop>
<D:displayname>{}</D:displayname>
<D:getlastmodified>{}</D:getlastmodified>
<D:resourcetype><D:collection/></D:resourcetype>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
</D:response>"#,
                href, displayname, mtime
            ),
            PathType::File | PathType::SymlinkFile => format!(
                r#"<D:response>
<D:href>{}</D:href>
<D:propstat>
<D:prop>
<D:displayname>{}</D:displayname>
<D:getcontentlength>{}</D:getcontentlength>
<D:getlastmodified>{}</D:getlastmodified>
<D:resourcetype></D:resourcetype>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
</D:response>"#,
                href,
                displayname,
                self.size.unwrap_or_default(),
                mtime
            ),
        }
    }
    fn base_name(&self) -> &str {
        Path::new(&self.name)
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
    }
}

#[derive(Debug, Serialize, Eq, PartialEq, Ord, PartialOrd)]
enum PathType {
    Dir,
    SymlinkDir,
    File,
    SymlinkFile,
}

fn to_timestamp(time: &SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn normalize_path<P: AsRef<Path>>(path: P) -> String {
    let path = path.as_ref().to_str().unwrap_or_default();
    if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    }
}

async fn ensure_path_parent(path: &Path) -> BoxResult<()> {
    if let Some(parent) = path.parent() {
        if fs::symlink_metadata(parent).await.is_err() {
            fs::create_dir_all(&parent).await?;
        }
    }
    Ok(())
}

fn add_cors(res: &mut Response) {
    res.headers_mut()
        .typed_insert(AccessControlAllowOrigin::ANY);
    res.headers_mut()
        .typed_insert(AccessControlAllowCredentials);

    res.headers_mut().typed_insert(
        vec![RANGE, CONTENT_TYPE, ACCEPT, ORIGIN, WWW_AUTHENTICATE]
            .into_iter()
            .collect::<AccessControlAllowHeaders>(),
    );
}

fn res_multistatus(res: &mut Response, content: &str) {
    *res.status_mut() = StatusCode::MULTI_STATUS;
    res.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    *res.body_mut() = Body::from(format!(
        r#"<?xml version="1.0" encoding="utf-8" ?>
<D:multistatus xmlns:D="DAV:">
{}
</D:multistatus>"#,
        content,
    ));
}

async fn zip_dir<W: AsyncWrite + Unpin>(writer: &mut W, dir: &Path) -> BoxResult<()> {
    let mut writer = ZipFileWriter::new(writer);
    let mut walkdir = WalkDir::new(dir);
    while let Some(entry) = walkdir.next().await {
        if let Ok(entry) = entry {
            let entry_path = entry.path();
            let meta = match fs::symlink_metadata(entry.path()).await {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            let filename = match entry_path.strip_prefix(dir).ok().and_then(|v| v.to_str()) {
                Some(v) => v,
                None => continue,
            };
            let entry_options = EntryOptions::new(filename.to_owned(), Compression::Deflate);
            let mut file = File::open(&entry_path).await?;
            let mut file_writer = writer.write_entry_stream(entry_options).await?;
            io::copy(&mut file, &mut file_writer).await?;
            file_writer.close().await?;
        }
    }
    writer.close().await?;
    Ok(())
}

fn extract_cache_headers(meta: &Metadata) -> Option<(ETag, LastModified)> {
    let mtime = meta.modified().ok()?;
    let timestamp = to_timestamp(&mtime);
    let size = meta.len();
    let etag = format!(r#""{}-{}""#, timestamp, size)
        .parse::<ETag>()
        .unwrap();
    let last_modified = LastModified::from(mtime);
    Some((etag, last_modified))
}

#[derive(Debug)]
struct RangeValue {
    start: u64,
    end: Option<u64>,
}

fn parse_range(headers: &HeaderMap<HeaderValue>) -> Option<RangeValue> {
    let range_hdr = headers.get(RANGE)?;
    let hdr = range_hdr.to_str().ok()?;
    let mut sp = hdr.splitn(2, '=');
    let units = sp.next().unwrap();
    if units == "bytes" {
        let range = sp.next()?;
        let mut sp_range = range.splitn(2, '-');
        let start: u64 = sp_range.next().unwrap().parse().ok()?;
        let end: Option<u64> = if let Some(end) = sp_range.next() {
            if end.is_empty() {
                None
            } else {
                Some(end.parse().ok()?)
            }
        } else {
            None
        };
        Some(RangeValue { start, end })
    } else {
        None
    }
}

fn status_forbid(res: &mut Response) {
    *res.status_mut() = StatusCode::FORBIDDEN;
    *res.body_mut() = Body::from("Forbidden");
}

fn status_not_found(res: &mut Response) {
    *res.status_mut() = StatusCode::NOT_FOUND;
    *res.body_mut() = Body::from("Not Found");
}

fn status_no_content(res: &mut Response) {
    *res.status_mut() = StatusCode::NO_CONTENT;
}

fn get_file_name(path: &Path) -> BoxResult<&str> {
    path.file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| format!("Failed to get file name of `{}`", path.display()).into())
}

fn set_webdav_headers(res: &mut Response) {
    res.headers_mut().insert(
        "Allow",
        HeaderValue::from_static("GET,HEAD,PUT,OPTIONS,DELETE,PROPFIND,COPY,MOVE"),
    );
    res.headers_mut()
        .insert("DAV", HeaderValue::from_static("1,2"));
}
