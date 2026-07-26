use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::i18n;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    pub repo_id: String,
    pub repo_type: String,
    pub revision: String,
    pub subpath: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatus {
    pub status: String, // "logged_in" | "invalid" | "missing"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Parse a HF web URL or bare `owner/repo` into RepoInfo.
pub fn parse_repo_input(text: &str, lang: &str) -> Result<RepoInfo, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(i18n::t("err_empty", lang));
    }

    let mut repo_type = "model".to_string();
    let mut revision = "main".to_string();
    let mut subpath = String::new();

    if text.starts_with("http://") || text.starts_with("https://") {
        let parsed = Url::parse(text).map_err(|_| i18n::t("err_parse_url", lang).to_string())?;
        let parts: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();

        let mut parts = parts;
        if let Some(first) = parts.first() {
            match *first {
                "datasets" => {
                    repo_type = "dataset".to_string();
                    parts.remove(0);
                }
                "spaces" => {
                    repo_type = "space".to_string();
                    parts.remove(0);
                }
                "models" => {
                    repo_type = "model".to_string();
                    parts.remove(0);
                }
                _ => {}
            }
        }

        if parts.len() < 2 {
            return Err(i18n::t("err_repo_url", lang).to_string());
        }

        let owner = parts[0];
        let repo = parts[1];
        let rest = &parts[2..];

        if !rest.is_empty() && rest[0] == "tree" {
            let rest = &rest[1..];
            if !rest.is_empty() {
                revision = rest[0].to_string();
                subpath = rest[1..].join("/");
            }
        } else if !rest.is_empty() {
            subpath = rest.join("/");
        }

        Ok(RepoInfo {
            repo_id: format!("{}/{}", owner, repo),
            repo_type,
            revision,
            subpath,
        })
    } else {
        let re = Regex::new(r"^([^/@]+)/([^/@]+)(?:@([^/]+))?(?:/(.*))?$")
            .unwrap();
        let caps = re
            .captures(text)
            .ok_or_else(|| i18n::t("err_parse_repo", lang).to_string())?;

        let owner = caps.get(1).unwrap().as_str();
        let repo = caps.get(2).unwrap().as_str();
        if let Some(rev) = caps.get(3) {
            revision = rev.as_str().to_string();
        }
        if let Some(sub) = caps.get(4) {
            subpath = sub.as_str().to_string();
        }

        Ok(RepoInfo {
            repo_id: format!("{}/{}", owner, repo),
            repo_type,
            revision,
            subpath,
        })
    }
}

/// Build the HF API base URL.
fn api_base(endpoint: &str) -> String {
    if endpoint.is_empty() {
        "https://huggingface.co".to_string()
    } else {
        endpoint.trim_end_matches('/').to_string()
    }
}

/// List files in a repo using the HF API.
pub async fn list_files(
    repo_id: &str,
    revision: &str,
    subpath: &str,
    repo_type: &str,
    endpoint: &str,
    lang: &str,
) -> Result<Vec<FileEntry>, String> {
    let base = api_base(endpoint);
    let type_prefix = match repo_type {
        "model" => "models",
        "dataset" => "datasets",
        "space" => "spaces",
        _ => "models",
    };

    let url = format!(
        "{}/api/{}/{}/tree/{}?recursive=true",
        base, type_prefix, repo_id, revision
    );
    let url = if subpath.is_empty() {
        url
    } else {
        format!("{}&path={}", url, subpath)
    };

    let client = reqwest::Client::new();
    let mut req = client
        .get(&url)
        .header("User-Agent", "hf-downloader-egui/0.1");

    // Attach token if available
    if let Some(token) = read_token() {
        req = req.bearer_auth(token);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("{}: {}", i18n::t("err_request", lang), e))?;

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(format!("{}: {}", i18n::t("err_repo_not_found", lang), repo_id));
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!("{}: {}", i18n::t("err_gated", lang), repo_id));
    }
    if !status.is_success() {
        return Err(format!("{}: {}", i18n::t("err_api", lang), status));
    }

    // The tree API returns an array of items
    let items: Vec<serde_json::Value> = resp
        .json()
        .await
        .map_err(|e| format!("{}: {}", i18n::t("err_parse_resp", lang), e))?;

    let mut files = Vec::new();
    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "file" {
            let path = item
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let size = item
                .get("size")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if !path.is_empty() {
                files.push(FileEntry { path, size });
            }
        }
    }

    Ok(files)
}

/// Compute target directory for a repo.
pub fn target_dir_for(repo_id: &str, base_dir: &str) -> String {
    let safe = repo_id.replace('/', "-");
    std::path::Path::new(base_dir)
        .join(&safe)
        .to_string_lossy()
        .to_string()
}

// --- Token management ---

fn token_path() -> Option<std::path::PathBuf> {
    // HF stores token at ~/.cache/huggingface/token
    dirs::home_dir().map(|h| {
        if cfg!(windows) {
            // On Windows, huggingface_hub uses USERPROFILE/.cache/huggingface/token
            h.join(".cache").join("huggingface").join("token")
        } else {
            h.join(".cache").join("huggingface").join("token")
        }
    })
}

pub fn read_token() -> Option<String> {
    token_path().and_then(|p| {
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// Check HF token status by calling whoami API.
pub async fn check_token(endpoint: &str, lang: &str) -> TokenStatus {
    let token = match read_token() {
        Some(t) => t,
        None => {
            return TokenStatus {
                status: "missing".to_string(),
                error: None,
            }
        }
    };

    let base = api_base(endpoint);
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/whoami-v2", base))
        .bearer_auth(&token)
        .header("User-Agent", "hf-downloader-egui/0.1")
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => TokenStatus {
            status: "logged_in".to_string(),
            error: None,
        },
        Ok(r) => TokenStatus {
            status: "invalid".to_string(),
            error: Some(format!("HTTP {}", r.status())),
        },
        Err(e) => TokenStatus {
            status: "invalid".to_string(),
            error: Some(format!("{}: {}", i18n::t("err_token_invalid", lang), e)),
        },
    }
}

/// Login by saving token to the standard HF location.
pub async fn login_token(token: &str, endpoint: &str, lang: &str) -> TokenStatus {
    let token = token.trim();
    if token.is_empty() {
        return TokenStatus {
            status: "missing".to_string(),
            error: Some(i18n::t("err_token_empty", lang)),
        };
    }

    // Verify token first
    let base = api_base(endpoint);
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/whoami-v2", base))
        .bearer_auth(token)
        .header("User-Agent", "hf-downloader-egui/0.1")
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            // Save token
            if let Some(path) = token_path() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&path, token);
            }
            TokenStatus {
                status: "logged_in".to_string(),
                error: None,
            }
        }
        Ok(r) => TokenStatus {
            status: "invalid".to_string(),
            error: Some(format!("{}: {}", i18n::t("err_token_invalid", lang), r.status())),
        },
        Err(e) => TokenStatus {
            status: "invalid".to_string(),
            error: Some(format!("{}: {}", i18n::t("err_token_invalid", lang), e)),
        },
    }
}
