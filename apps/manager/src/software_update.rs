#[cfg(any(not(feature = "update-qa"), test))]
use reqwest::Url;
#[cfg(not(feature = "update-qa"))]
use reqwest::{
    blocking::{Client, Response},
    header::{ACCEPT, LOCATION},
    redirect::Policy,
};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const DEFAULT_REPO: &str = "blackwhite-poi/Action75-Wallpaper";
#[cfg(not(feature = "update-qa"))]
const GITEE_REPO: &str = "black_and_white_poi/Action75-Wallpaper";
#[cfg(not(feature = "update-qa"))]
const GITEE_MANIFEST: &str = "Action75-Wallpaper-Update.json";
const METADATA_LIMIT: u64 = 2 * 1024 * 1024;
const ASSET_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const TEMP_PREFIX: &str = "Action75-update-";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    name: String,
    #[cfg(not(feature = "update-qa"))]
    url: String,
    #[cfg(not(feature = "update-qa"))]
    mirror_url: Option<String>,
    size: u64,
    digest: [u8; 32],
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: Option<String>,
    browser_download_url: Option<String>,
    size: Option<u64>,
    digest: Option<String>,
}

fn repository() -> Result<&'static str, String> {
    let repo = option_env!("ACTION75_UPDATE_REPO").unwrap_or(DEFAULT_REPO);
    let (owner, name) = repo.split_once('/').ok_or("官方更新仓库配置无效")?;
    let valid = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
    };
    if valid(owner) && valid(name) {
        Ok(repo)
    } else {
        Err("官方更新仓库配置无效".into())
    }
}

fn newer_version<'a>(tag: &'a str, installed: &str) -> Result<Option<&'a str>, String> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let next = Version::parse(version).map_err(|_| "官方发布标签不是有效版本号")?;
    let current = Version::parse(installed).map_err(|_| "当前软件版本号无效")?;
    if !next.pre.is_empty() || !next.build.is_empty() || !next.cmp_precedence(&current).is_gt() {
        return Ok(None);
    }
    Ok(Some(version))
}

fn select(metadata: &[u8], repo: &str, installed: &str) -> Result<Option<Release>, String> {
    let release: GithubRelease =
        serde_json::from_slice(metadata).map_err(|_| "官方发布信息格式无效")?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(version) = newer_version(&release.tag_name, installed)? else {
        return Ok(None);
    };
    let name = format!("Action75-Wallpaper-Setup-{version}-x64.exe");
    let mut matching = release.assets.iter().filter(|a| a.name == name);
    let asset = matching.next().ok_or("该版本缺少 Windows x64 安装包")?;
    if matching.next().is_some() || asset.state.as_deref() != Some("uploaded") {
        return Err("官方安装包名称重复或上传未完成".into());
    }
    let size = asset
        .size
        .filter(|n| (1..=ASSET_LIMIT).contains(n))
        .ok_or("官方安装包大小无效")?;
    let url = format!(
        "https://github.com/{repo}/releases/download/{}/{name}",
        release.tag_name
    );
    if asset.browser_download_url.as_deref() != Some(&url) {
        return Err("安装包地址与官方仓库不符".into());
    }
    let digest = parse_digest(asset.digest.as_deref())?;
    Ok(Some(Release {
        version: version.to_owned(),
        #[cfg(not(feature = "update-qa"))]
        mirror_url: (repo == DEFAULT_REPO).then(|| gitee_release_url(&release.tag_name, &name)),
        name,
        #[cfg(not(feature = "update-qa"))]
        url,
        size,
        digest,
    }))
}

fn parse_digest(value: Option<&str>) -> Result<[u8; 32], String> {
    let hex = value
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("官方发布缺少有效的 SHA-256 校验值")?;
    let mut digest = [0; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| "SHA-256 校验值无效")?;
    }
    Ok(digest)
}

#[cfg(not(feature = "update-qa"))]
fn client(timeout: Duration) -> Result<Client, String> {
    Client::builder()
        .https_only(true)
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(12))
        .timeout(timeout)
        .user_agent(concat!("Action75-Wallpaper/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "无法初始化安全更新连接".into())
}

#[cfg(not(feature = "update-qa"))]
fn status_error(code: u16) -> String {
    match code {
        404 => "官方发布尚不可用（仓库或发行版不存在）".into(),
        403 | 429 => "官方更新服务暂时限制请求，请稍后重试".into(),
        _ => format!("官方更新服务返回 HTTP {code}"),
    }
}

fn read_metadata(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "读取官方发布信息失败")?;
    if bytes.len() as u64 > METADATA_LIMIT {
        return Err("官方发布信息超过大小限制".into());
    }
    Ok(bytes)
}

#[cfg(feature = "update-qa")]
fn fixture() -> Result<PathBuf, String> {
    option_env!("ACTION75_QA_UPDATE_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| "离线测试构建未指定编译时回放目录".into())
}

pub fn check() -> Result<Option<Release>, String> {
    cleanup_downloads();
    let repo = repository()?;
    let installed = env!("CARGO_PKG_VERSION");
    #[cfg(feature = "update-qa")]
    {
        let bytes = read_metadata(
            fs::File::open(fixture()?.join("release.json")).map_err(|_| "读取测试发行信息失败")?,
        )?;
        select(&bytes, repo, installed)
    }
    #[cfg(not(feature = "update-qa"))]
    {
        let client = client(Duration::from_secs(25))?;
        let github = client
            .get(format!(
                "https://api.github.com/repos/{repo}/releases/latest"
            ))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .map_err(|_| "无法连接 GitHub 更新服务".to_owned())
            .and_then(read_release_response)
            .and_then(|bytes| select(&bytes, repo, installed));
        match github {
            Ok(release) => Ok(release),
            Err(error) if repo == DEFAULT_REPO => match check_gitee(&client, installed) {
                Ok(Some(release)) => Ok(Some(release)),
                Ok(None) => Err(format!("{error}；Gitee 镜像暂未发现比当前版本更新的安装包")),
                Err(mirror) => Err(format!("{error}；Gitee 镜像：{mirror}")),
            },
            Err(error) => Err(error),
        }
    }
}

#[cfg(not(feature = "update-qa"))]
fn read_release_response(response: Response) -> Result<Vec<u8>, String> {
    if response.status().as_u16() != 200 {
        return Err(status_error(response.status().as_u16()));
    }
    read_metadata(response)
}

#[cfg(not(feature = "update-qa"))]
fn gitee_release_url(tag: &str, name: &str) -> String {
    format!("https://gitee.com/{GITEE_REPO}/releases/download/{tag}/{name}")
}

#[cfg(not(feature = "update-qa"))]
#[derive(Deserialize)]
struct GiteeManifest {
    tag_name: String,
    size: u64,
    digest: String,
}

#[cfg(not(feature = "update-qa"))]
fn select_gitee(metadata: &[u8], tag: &str, version: &str) -> Result<Release, String> {
    let manifest: GiteeManifest =
        serde_json::from_slice(metadata).map_err(|_| "镜像校验清单格式无效")?;
    if manifest.tag_name != tag || !(1..=ASSET_LIMIT).contains(&manifest.size) {
        return Err("镜像校验清单与正式版本不符".into());
    }
    let name = format!("Action75-Wallpaper-Setup-{version}-x64.exe");
    Ok(Release {
        version: version.to_owned(),
        url: gitee_release_url(tag, &name),
        mirror_url: None,
        name,
        size: manifest.size,
        digest: parse_digest(Some(&manifest.digest))?,
    })
}

#[cfg(not(feature = "update-qa"))]
fn check_gitee(client: &Client, installed: &str) -> Result<Option<Release>, String> {
    #[derive(Deserialize)]
    struct MirrorRelease {
        tag_name: String,
        prerelease: bool,
        assets: Vec<Asset>,
    }
    let bytes = read_release_response(
        client
            .get(format!(
                "https://gitee.com/api/v5/repos/{GITEE_REPO}/releases/latest"
            ))
            .send()
            .map_err(|_| "无法连接更新服务")?,
    )?;
    let latest: MirrorRelease = serde_json::from_slice(&bytes).map_err(|_| "发布信息格式无效")?;
    if latest.prerelease {
        return Ok(None);
    }
    let Some(version) = newer_version(&latest.tag_name, installed)? else {
        return Ok(None);
    };
    let manifest_url = gitee_release_url(&latest.tag_name, GITEE_MANIFEST);
    let has_asset = |name: &str, url: &str| {
        let mut matches = latest.assets.iter().filter(|asset| asset.name == name);
        matches
            .next()
            .and_then(|asset| asset.browser_download_url.as_deref())
            == Some(url)
            && matches.next().is_none()
    };
    if !has_asset(GITEE_MANIFEST, &manifest_url) {
        return Err("镜像尚未上传该版本的校验清单".into());
    }
    let installer = format!("Action75-Wallpaper-Setup-{version}-x64.exe");
    if !has_asset(&installer, &gitee_release_url(&latest.tag_name, &installer)) {
        return Err("镜像缺少该版本的正式安装包".into());
    }
    let response = request_asset(client, &manifest_url, None)?;
    select_gitee(&read_metadata(response)?, &latest.tag_name, version).map(Some)
}

#[cfg(any(not(feature = "update-qa"), test))]
fn redirect_target(previous: &Url, location: &str) -> Result<Url, String> {
    let url = previous.join(location).map_err(|_| "下载重定向地址无效")?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(
            url.host_str(),
            Some(
                "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
                    | "github-releases.githubusercontent.com"
            )
        )
    {
        return Err("已阻止来自非官方地址的下载重定向".into());
    }
    Ok(url)
}

#[cfg(not(feature = "update-qa"))]
fn gitee_redirect(previous: &Url, location: &str) -> Result<Url, String> {
    let url = previous.join(location).map_err(|_| "镜像跳转地址无效")?;
    let allowed = match url.host_str() {
        Some("gitee.com") => url
            .path()
            .starts_with(&format!("/{GITEE_REPO}/attach_files/")),
        Some("foruda.gitee.com") => url.path().starts_with("/attach_file/"),
        _ => false,
    };
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !allowed
    {
        return Err("已阻止来自非官方地址的镜像跳转".into());
    }
    Ok(url)
}

#[cfg(not(feature = "update-qa"))]
fn request_asset(client: &Client, source: &str, size: Option<u64>) -> Result<Response, String> {
    let mut url = Url::parse(source).map_err(|_| "官方下载地址无效")?;
    let redirect = if url.host_str() == Some("gitee.com") {
        gitee_redirect
    } else {
        redirect_target
    };
    for _ in 0..=4 {
        let response = client
            .get(url.clone())
            .send()
            .map_err(|_| "连接官方安装包下载服务失败")?;
        match response.status().as_u16() {
            200 => {
                if response
                    .content_length()
                    .zip(size)
                    .is_some_and(|(len, expected)| len != expected)
                {
                    return Err("安装包下载长度与官方信息不符".into());
                }
                return Ok(response);
            }
            301 | 302 | 303 | 307 | 308 => {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|s| s.to_str().ok())
                    .ok_or("下载重定向地址缺失")?;
                url = redirect(&url, location)?;
            }
            status => return Err(status_error(status)),
        }
    }
    Err("安装包下载重定向次数过多".into())
}

fn verify_stream(
    mut reader: impl Read,
    mut output: impl Write,
    release: &Release,
) -> Result<(), String> {
    let mut hash = Sha256::new();
    let mut written = 0u64;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut chunk)
            .map_err(|_| "读取安装包失败，请重试")?;
        if count == 0 {
            break;
        }
        written += count as u64;
        if written > release.size {
            return Err("下载长度超出官方安装包大小".into());
        }
        hash.update(&chunk[..count]);
        output
            .write_all(&chunk[..count])
            .map_err(|_| "写入安装包失败，请检查剩余空间")?;
    }
    if written != release.size || <[u8; 32]>::from(hash.finalize()) != release.digest {
        return Err("安装包长度或 SHA-256 校验失败".into());
    }
    Ok(())
}

/// Verify the exact file held open and deny write/delete sharing until the
/// caller drops the handle after handing off to the installer.
pub fn lock_verified(path: &Path, release: &Release) -> Result<fs::File, String> {
    use std::io::Seek;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };

    let mut file = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .map_err(|_| "安装包已移动、被占用或无法打开".to_string())?;
    let meta = file.metadata().map_err(|_| "无法读取安装包属性")?;
    if !meta.is_file()
        || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || meta.len() != release.size
    {
        return Err("安装包类型或长度已改变".into());
    }
    verify_stream(&mut file, std::io::sink(), release)?;
    file.rewind().map_err(|_| "无法重新定位已验证的安装包")?;
    Ok(file)
}

fn save_verified(
    reader: impl Read,
    release: &Release,
    directory: &Path,
) -> Result<PathBuf, String> {
    let mut output = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .suffix(".exe")
        .rand_bytes(16)
        .tempfile_in(directory)
        .map_err(|_| "无法创建安装包临时文件")?;
    verify_stream(reader, &mut output, release)?;
    output.as_file().sync_all().map_err(|_| "保存安装包失败")?;
    let (_, path) = output.keep().map_err(|_| "无法保留已校验的安装包")?;
    Ok(path)
}

pub fn download(release: &Release) -> Result<PathBuf, String> {
    // version is public for the UI; reject a caller-mutated display version.
    if release.name != format!("Action75-Wallpaper-Setup-{}-x64.exe", release.version) {
        return Err("安装包与所选版本不一致".into());
    }
    #[cfg(feature = "update-qa")]
    {
        let source =
            fs::File::open(fixture()?.join(&release.name)).map_err(|_| "读取测试安装包失败")?;
        save_verified(source, release, &std::env::temp_dir())
    }
    #[cfg(not(feature = "update-qa"))]
    {
        let client = client(Duration::from_secs(300))?;
        let download_from = |url| {
            let source = request_asset(&client, url, Some(release.size))?;
            save_verified(source, release, &std::env::temp_dir())
        };
        match (download_from(&release.url), release.mirror_url.as_deref()) {
            (Err(error), Some(mirror)) => download_from(mirror)
                .map_err(|mirror_error| format!("{error}；Gitee 镜像：{mirror_error}")),
            (result, _) => result,
        }
    }
}

// The active installer may still hold its .exe open. Never remove recent files
// or follow links. A later check retires leftover copies of our own downloads.
pub fn cleanup_downloads() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name
            .strip_prefix(TEMP_PREFIX)
            .and_then(|s| s.strip_suffix(".exe"))
        else {
            continue;
        };
        if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_file()
            && meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= Duration::from_secs(86400))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const REPO: &str = "blackwhite-poi/Action75-Wallpaper";
    fn metadata(tag: &str) -> Value {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        let name = format!("Action75-Wallpaper-Setup-{version}-x64.exe");
        json!({"draft":false,"prerelease":false,"tag_name":tag,"assets":[{
            "name":name,"state":"uploaded","size":3,
            "browser_download_url":format!("https://github.com/{REPO}/releases/download/{tag}/{name}"),
            "digest":format!("sha256:{:x}",Sha256::digest(b"abc"))
        }]})
    }
    fn inspect(value: &Value) -> Result<Option<Release>, String> {
        select(&serde_json::to_vec(value).unwrap(), REPO, "0.3.9")
    }
    #[test]
    fn stable_numeric_precedence() {
        for tag in ["v0.3.10", "0.4.0", "v1.0.0"] {
            assert!(inspect(&metadata(tag)).unwrap().is_some());
        }
        for tag in ["v0.3.8", "v0.3.9", "v0.4.0-rc.1", "v0.3.9+repacked"] {
            assert!(inspect(&metadata(tag)).unwrap().is_none());
        }
        for flag in ["draft", "prerelease"] {
            let mut m = metadata("v1.0.0");
            m[flag] = true.into();
            assert!(inspect(&m).unwrap().is_none());
        }
        assert!(inspect(&metadata("v1.0.0/../test")).is_err());
    }
    #[test]
    fn requires_one_exact_official_asset() {
        for (key, value) in [
            ("name", json!("Action75-Wallpaper-QA-Setup-0.4.0-x64.exe")),
            ("state", json!("new")),
            ("size", json!(0)),
            ("size", json!(ASSET_LIMIT + 1)),
            ("digest", Value::Null),
            ("digest", json!("sha256:bad")),
            ("digest", json!(format!("sha256:{}", "zz".repeat(32)))),
            (
                "browser_download_url",
                json!(
                    "https://github.com/another/project/releases/download/v0.4.0/Action75-Wallpaper-Setup-0.4.0-x64.exe"
                ),
            ),
        ] {
            let mut m = metadata("v0.4.0");
            m["assets"][0][key] = value;
            assert!(inspect(&m).is_err(), "{key}");
        }
        let mut m = metadata("v0.4.0");
        let duplicate = m["assets"][0].clone();
        m["assets"].as_array_mut().unwrap().push(duplicate);
        assert!(inspect(&m).is_err());
    }
    #[test]
    fn redirects_only_to_exact_https_asset_hosts() {
        let origin = Url::parse("https://github.com/org/repo/releases/download/v1/file").unwrap();
        for url in [
            "https://release-assets.githubusercontent.com/path?signature=private",
            "https://objects.githubusercontent.com/path",
        ] {
            assert!(redirect_target(&origin, url).is_ok());
        }
        for url in [
            "http://objects.githubusercontent.com/a",
            "https://release-assets.githubusercontent.com.other.tld/a",
            "https://other.tld/github.com",
            "https://foo@objects.githubusercontent.com/path",
            "https://objects.githubusercontent.com:8000/path",
            "file:///C:/Windows/test",
            "https://github.com/other/repo",
            "//api.github.com/path",
            "https://objects.githubusercontent.com/a#fragment",
        ] {
            assert!(redirect_target(&origin, url).is_err(), "{url}");
        }
    }
    #[cfg(not(feature = "update-qa"))]
    #[test]
    fn mirror_uses_only_the_official_release_and_verified_bytes() {
        let github = inspect(&metadata("v0.4.0")).unwrap().unwrap();
        let data = json!({
            "tag_name": "v0.4.0", "size": 3,
            "digest": format!("sha256:{:x}", Sha256::digest(b"abc")),
        });
        let bytes = serde_json::to_vec(&data).unwrap();
        let mirror = select_gitee(&bytes, "v0.4.0", "0.4.0").unwrap();
        assert_eq!(Some(&mirror.url), github.mirror_url.as_ref());
        assert!(select_gitee(&bytes, "v0.4.1", "0.4.1").is_err());
        assert!(verify_stream(&b"abd"[..], Vec::new(), &mirror).is_err());
        assert!(verify_stream(&b"abc"[..], Vec::new(), &mirror).is_ok());
        let origin = Url::parse(&mirror.url).unwrap();
        assert!(
            gitee_redirect(
                &origin,
                &format!("/{GITEE_REPO}/attach_files/1/download/a.exe")
            )
            .is_ok()
        );
        assert!(
            gitee_redirect(
                &origin,
                "https://foruda.gitee.com/attach_file/1/a.exe?token=example"
            )
            .is_ok()
        );
        for target in [
            "https://gitee.com/other/project/attach_files/1/download/a.exe",
            "https://foruda.gitee.com.example.com/attach_file/1/a.exe",
            "http://foruda.gitee.com/attach_file/1/a.exe",
        ] {
            assert!(gitee_redirect(&origin, target).is_err());
        }
    }
    #[test]
    fn streamed_installers_checked_before_a_path_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let r = inspect(&metadata("v0.4.0")).unwrap().unwrap();
        for input in [b"ab".as_slice(), b"abd", b"abcd"] {
            assert!(save_verified(input, &r, dir.path()).is_err());
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        }
        let first = save_verified(&b"abc"[..], &r, dir.path()).unwrap();
        let second = save_verified(&b"abc"[..], &r, dir.path()).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(first).unwrap(), b"abc");
        assert_eq!(second.extension().unwrap(), "exe");
    }
    #[test]
    fn locked_handoff_refuses_changes_and_permits_readers() {
        let dir = tempfile::tempdir().unwrap();
        let r = inspect(&metadata("v0.4.0")).unwrap().unwrap();
        let path = save_verified(&b"abc"[..], &r, dir.path()).unwrap();
        let moved = path.with_extension("moved");
        let lock = lock_verified(&path, &r).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        for err in [
            fs::OpenOptions::new().write(true).open(&path).unwrap_err(),
            fs::remove_file(&path).unwrap_err(),
            fs::rename(&path, &moved).unwrap_err(),
        ] {
            assert_eq!(
                err.raw_os_error(),
                Some(32),
                "expected Windows sharing violation"
            );
        }
        drop(lock);
        fs::write(&path, b"xyz").unwrap();
        assert!(lock_verified(&path, &r).is_err());
        fs::write(&path, b"shorter?").unwrap();
        assert!(lock_verified(&path, &r).is_err());
        fs::rename(&path, &moved).unwrap();
    }

    #[test]
    fn handoff_requires_exclusive_access_against_existing_writers() {
        let dir = tempfile::tempdir().unwrap();
        let r = inspect(&metadata("v0.4.0")).unwrap().unwrap();
        let path = save_verified(&b"abc"[..], &r, dir.path()).unwrap();
        let writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(lock_verified(&path, &r).is_err());
        drop(writer);
        assert!(lock_verified(&path, &r).is_ok());
    }
    #[test]
    fn oversized_metadata_rejected() {
        assert!(read_metadata(std::io::repeat(b' ').take(METADATA_LIMIT + 1)).is_err());
    }
}
