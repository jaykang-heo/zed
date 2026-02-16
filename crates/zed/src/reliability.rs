use anyhow::{Context as _, Result};
use client::{
    Client,
    telemetry::{MINIDUMP_ENDPOINT, SENTRY_DSN},
};
use futures::{AsyncReadExt, TryStreamExt};
use gpui::{App, AppContext as _, SerializedThreadTaskTimings};
use http_client::{self, AsyncBody, HttpClient, Request};
use log::info;
use project::Project;
use proto::{CrashReport, GetCrashFilesResponse};
use reqwest::{
    Method,
    multipart::{Form, Part},
};
use smol::stream::StreamExt;
use std::{ffi::OsStr, fs, sync::Arc, thread::ThreadId, time::Duration};
use util::ResultExt;

use crate::STARTUP_TIME;

const MAX_HANG_TRACES: usize = 3;

pub fn init(client: Arc<Client>, cx: &mut App) {
    init_soft_unreachable_reporter(client.clone(), cx);
    monitor_hangs(cx);

    if client.telemetry().diagnostics_enabled() {
        let client = client.clone();
        cx.background_spawn(async move {
            upload_previous_minidumps(client).await.warn_on_err();
        })
        .detach()
    }

    cx.observe_new(move |project: &mut Project, _, cx| {
        let client = client.clone();

        let Some(remote_client) = project.remote_client() else {
            return;
        };
        remote_client.update(cx, |remote_client, cx| {
            if !client.telemetry().diagnostics_enabled() {
                return;
            }
            let request = remote_client
                .proto_client()
                .request(proto::GetCrashFiles {});
            cx.background_spawn(async move {
                let GetCrashFilesResponse { crashes } = request.await?;

                let Some(endpoint) = MINIDUMP_ENDPOINT.as_ref() else {
                    return Ok(());
                };
                for CrashReport {
                    metadata,
                    minidump_contents,
                } in crashes
                {
                    if let Some(metadata) = serde_json::from_str(&metadata).log_err() {
                        upload_minidump(client.clone(), endpoint, minidump_contents, &metadata)
                            .await
                            .log_err();
                    }
                }

                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        })
    })
    .detach();
}

/// Metadata captured at init time for inclusion in Sentry events.
struct SentryEventMetadata {
    commit_sha: String,
    zed_version: String,
    release_channel: String,
    binary: String,
    os_name: String,
    os_version: String,
    architecture: &'static str,
}

/// A message sent from `soft_unreachable!` call sites to the background sender.
struct SoftUnreachableEvent {
    message: String,
    backtrace: String,
    file: &'static str,
    line: u32,
    timestamp: chrono::DateTime<chrono::Utc>,
}

/// Parses a Sentry DSN of the form `https://{public_key}@{host}/{project_id}`
/// into a store endpoint URL: `https://{host}/api/{project_id}/store/`
/// and the public key (sentry_key).
fn parse_sentry_dsn(dsn: &str) -> Option<(String, String)> {
    let url = url::Url::parse(dsn).ok()?;
    let public_key = url.username().to_string();
    if public_key.is_empty() {
        return None;
    }
    let host = url.host_str()?;
    let port_suffix = url.port().map(|p| format!(":{}", p)).unwrap_or_default();
    let scheme = url.scheme();

    // The project ID is the last path segment
    let project_id = url.path().trim_start_matches('/');
    if project_id.is_empty() {
        return None;
    }

    let store_url = format!(
        "{}://{}{}/api/{}/store/",
        scheme, host, port_suffix, project_id
    );
    Some((store_url, public_key))
}

fn build_sentry_event_json(
    event: &SoftUnreachableEvent,
    metadata: &SentryEventMetadata,
    user_id: Option<String>,
    is_staff: Option<bool>,
) -> serde_json::Value {
    let event_id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let timestamp = event.timestamp.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string();
    let mut payload = serde_json::json!({
        "event_id": event_id,
        "timestamp": timestamp,
        "level": "error",
        "platform": "rust",
        "logger": "soft_unreachable",
        "release": metadata.commit_sha,
        "fingerprint": ["soft_unreachable", &event.file, event.line.to_string()],
        "tags": {
            "channel": metadata.release_channel,
            "version": metadata.zed_version,
            "binary": metadata.binary,
        },
        "contexts": {
            "os": {
                "type": "os",
                "name": metadata.os_name,
                "version": metadata.os_version,
            },
            "device": {
                "type": "device",
                "arch": metadata.architecture,
            },
        },
        "logentry": {
            "formatted": event.message,
        },
        "extra": {
            "file": event.file,
            "line": event.line,
            "backtrace": event.backtrace,
        },
    });

    if let Some(id) = user_id {
        let mut user = serde_json::json!({ "id": id });
        if let Some(staff) = is_staff {
            user["is_staff"] = serde_json::Value::String(staff.to_string());
        }
        payload["user"] = user;
    }

    payload
}

/// Initialize the soft_unreachable reporter.
fn init_soft_unreachable_reporter(client: Arc<Client>, cx: &mut App) {
    // Only report if diagnostics are enabled and we have a Sentry DSN configured.
    if !client.telemetry().diagnostics_enabled() {
        return;
    }

    let Some(dsn) = SENTRY_DSN.as_ref() else {
        log::debug!("ZED_SENTRY_DSN not set, soft_unreachable events will only be logged locally");
        return;
    };

    let Some((store_url, sentry_key)) = parse_sentry_dsn(dsn) else {
        log::warn!("Failed to parse ZED_SENTRY_DSN, soft_unreachable Sentry reporting disabled");
        return;
    };

    let os_version = client::telemetry::os_version();

    let metadata = SentryEventMetadata {
        commit_sha: release_channel::AppCommitSha::try_global(cx)
            .map(|sha| sha.full())
            .unwrap_or_else(|| "unknown".to_owned()),
        zed_version: release_channel::AppVersion::global(cx).to_string(),
        release_channel: release_channel::RELEASE_CHANNEL_NAME.clone(),
        binary: "zed".to_owned(),
        os_name: client::telemetry::os_name(),
        os_version,
        architecture: std::env::consts::ARCH,
    };

    let (tx, mut rx) = futures::channel::mpsc::unbounded::<SoftUnreachableEvent>();

    // Register the global reporter callback in `util`, capturing the sender
    // directly in the closure.
    util::set_soft_unreachable_reporter(move |message, backtrace, file, line| {
        tx.unbounded_send(SoftUnreachableEvent {
            message,
            backtrace,
            file,
            line,
            timestamp: chrono::Utc::now(),
        })
        .ok();
    });

    let http_client = client.http_client();
    let telemetry = client.telemetry().clone();

    // Spawn a background task that drains the channel and sends events to Sentry.
    cx.background_spawn(async move {
        while let Some(event) = futures::StreamExt::next(&mut rx).await {
            let user_id = telemetry
                .metrics_id()
                .map(|id| id.to_string())
                .or_else(|| {
                    telemetry
                        .installation_id()
                        .map(|id| format!("installation-{}", id))
                });
            let is_staff = telemetry.is_staff();

            let payload = build_sentry_event_json(&event, &metadata, user_id, is_staff);

            let body = match serde_json::to_vec(&payload) {
                Ok(b) => b,
                Err(e) => {
                    log::error!("Failed to serialize soft_unreachable Sentry event: {e}");
                    continue;
                }
            };

            let req = match Request::builder()
                .method(Method::POST)
                .uri(&store_url)
                .header("Content-Type", "application/json")
                .header(
                    "X-Sentry-Auth",
                    format!(
                        "Sentry sentry_version=7, sentry_client=zed-soft-unreachable/1.0, sentry_key={}",
                        sentry_key
                    ),
                )
                .body(AsyncBody::from(body))
            {
                Ok(r) => r,
                Err(e) => {
                    log::error!("Failed to build soft_unreachable Sentry request: {e}");
                    continue;
                }
            };

            match async {
                let mut response = http_client.send(req).await?;
                let mut response_text = String::new();
                response
                    .body_mut()
                    .read_to_string(&mut response_text)
                    .await?;
                if !response.status().is_success() {
                    anyhow::bail!(
                        "Sentry store returned {}: {}",
                        response.status(),
                        response_text
                    );
                }
                anyhow::Ok(response_text)
            }
            .await
            {
                Ok(response_text) => {
                    log::info!(
                        "Reported soft_unreachable to Sentry ({}:{}): event {}",
                        event.file,
                        event.line,
                        response_text
                    );
                }
                Err(e) => {
                    log::error!("Failed to report soft_unreachable to Sentry: {e}");
                }
            }
        }
        log::debug!("soft_unreachable Sentry reporter task exiting");
    })
    .detach();
}

fn monitor_hangs(cx: &App) {
    let main_thread_id = std::thread::current().id();

    let foreground_executor = cx.foreground_executor();
    let background_executor = cx.background_executor();

    // 3 seconds hang
    let (mut tx, mut rx) = futures::channel::mpsc::channel(3);
    foreground_executor
        .spawn(async move { while (rx.next().await).is_some() {} })
        .detach();

    background_executor
        .spawn({
            let background_executor = background_executor.clone();
            async move {
                cleanup_old_hang_traces();

                let mut hang_time = None;

                let mut hanging = false;
                loop {
                    background_executor.timer(Duration::from_secs(1)).await;
                    match tx.try_send(()) {
                        Ok(_) => {
                            hang_time = None;
                            hanging = false;
                            continue;
                        }
                        Err(e) => {
                            let is_full = e.into_send_error().is_full();
                            if is_full && !hanging {
                                hanging = true;
                                hang_time = Some(chrono::Local::now());
                            }

                            if is_full {
                                save_hang_trace(
                                    main_thread_id,
                                    &background_executor,
                                    hang_time.unwrap(),
                                );
                            }
                        }
                    }
                }
            }
        })
        .detach();
}

fn cleanup_old_hang_traces() {
    if let Ok(entries) = std::fs::read_dir(paths::hang_traces_dir()) {
        let mut files: Vec<_> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "miniprof")
            })
            .collect();

        if files.len() > MAX_HANG_TRACES {
            files.sort_by_key(|entry| entry.file_name());
            for entry in files.iter().take(files.len() - MAX_HANG_TRACES) {
                std::fs::remove_file(entry.path()).log_err();
            }
        }
    }
}

fn save_hang_trace(
    main_thread_id: ThreadId,
    background_executor: &gpui::BackgroundExecutor,
    hang_time: chrono::DateTime<chrono::Local>,
) {
    let thread_timings = background_executor.dispatcher().get_all_timings();
    let thread_timings = thread_timings
        .into_iter()
        .map(|mut timings| {
            if timings.thread_id == main_thread_id {
                timings.thread_name = Some("main".to_string());
            }

            SerializedThreadTaskTimings::convert(*STARTUP_TIME.get().unwrap(), timings)
        })
        .collect::<Vec<_>>();

    let trace_path = paths::hang_traces_dir().join(&format!(
        "hang-{}.miniprof",
        hang_time.format("%Y-%m-%d_%H-%M-%S")
    ));

    let Some(timings) = serde_json::to_string(&thread_timings)
        .context("hang timings serialization")
        .log_err()
    else {
        return;
    };

    if let Ok(entries) = std::fs::read_dir(paths::hang_traces_dir()) {
        let mut files: Vec<_> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "miniprof")
            })
            .collect();

        if files.len() >= MAX_HANG_TRACES {
            files.sort_by_key(|entry| entry.file_name());
            for entry in files.iter().take(files.len() - (MAX_HANG_TRACES - 1)) {
                std::fs::remove_file(entry.path()).log_err();
            }
        }
    }

    std::fs::write(&trace_path, timings)
        .context("hang trace file writing")
        .log_err();

    info!(
        "hang detected, trace file saved at: {}",
        trace_path.display()
    );
}

pub async fn upload_previous_minidumps(client: Arc<Client>) -> anyhow::Result<()> {
    let Some(minidump_endpoint) = MINIDUMP_ENDPOINT.as_ref() else {
        log::warn!("Minidump endpoint not set");
        return Ok(());
    };

    let mut children = smol::fs::read_dir(paths::logs_dir()).await?;
    while let Some(child) = children.next().await {
        let child = child?;
        let child_path = child.path();
        if child_path.extension() != Some(OsStr::new("dmp")) {
            continue;
        }
        let mut json_path = child_path.clone();
        json_path.set_extension("json");
        let Ok(metadata) = smol::fs::read(&json_path)
            .await
            .map_err(|e| anyhow::anyhow!(e))
            .and_then(|data| serde_json::from_slice(&data).map_err(|e| anyhow::anyhow!(e)))
        else {
            continue;
        };
        if upload_minidump(
            client.clone(),
            minidump_endpoint,
            smol::fs::read(&child_path)
                .await
                .context("Failed to read minidump")?,
            &metadata,
        )
        .await
        .log_err()
        .is_some()
        {
            fs::remove_file(child_path).ok();
            fs::remove_file(json_path).ok();
        }
    }
    Ok(())
}

async fn upload_minidump(
    client: Arc<Client>,
    endpoint: &str,
    minidump: Vec<u8>,
    metadata: &crashes::CrashInfo,
) -> Result<()> {
    let mut form = Form::new()
        .part(
            "upload_file_minidump",
            Part::bytes(minidump)
                .file_name("minidump.dmp")
                .mime_str("application/octet-stream")?,
        )
        .text(
            "sentry[tags][channel]",
            metadata.init.release_channel.clone(),
        )
        .text("sentry[tags][version]", metadata.init.zed_version.clone())
        .text("sentry[tags][binary]", metadata.init.binary.clone())
        .text("sentry[release]", metadata.init.commit_sha.clone())
        .text("platform", "rust");
    let mut panic_message = "".to_owned();
    if let Some(panic_info) = metadata.panic.as_ref() {
        panic_message = panic_info.message.clone();
        form = form
            .text("sentry[logentry][formatted]", panic_info.message.clone())
            .text("span", panic_info.span.clone());
    }
    if let Some(minidump_error) = metadata.minidump_error.clone() {
        form = form.text("minidump_error", minidump_error);
    }

    if let Some(id) = client.telemetry().metrics_id() {
        form = form.text("sentry[user][id]", id.to_string());
        form = form.text(
            "sentry[user][is_staff]",
            if client.telemetry().is_staff().unwrap_or_default() {
                "true"
            } else {
                "false"
            },
        );
    } else if let Some(id) = client.telemetry().installation_id() {
        form = form.text("sentry[user][id]", format!("installation-{}", id))
    }

    ::telemetry::event!(
        "Minidump Uploaded",
        panic_message = panic_message,
        crashed_version = metadata.init.zed_version.clone(),
        commit_sha = metadata.init.commit_sha.clone(),
    );

    let gpu_count = metadata.gpus.len();
    for (index, gpu) in metadata.gpus.iter().cloned().enumerate() {
        let system_specs::GpuInfo {
            device_name,
            device_pci_id,
            vendor_name,
            vendor_pci_id,
            driver_version,
            driver_name,
        } = gpu;
        let num = if gpu_count == 1 && metadata.active_gpu.is_none() {
            String::new()
        } else {
            index.to_string()
        };
        let name = format!("gpu{num}");
        let root = format!("sentry[contexts][{name}]");
        form = form
            .text(
                format!("{root}[Description]"),
                "A GPU found on the users system. May or may not be the GPU Zed is running on",
            )
            .text(format!("{root}[type]"), "gpu")
            .text(format!("{root}[name]"), device_name.unwrap_or(name))
            .text(format!("{root}[id]"), format!("{:#06x}", device_pci_id))
            .text(
                format!("{root}[vendor_id]"),
                format!("{:#06x}", vendor_pci_id),
            )
            .text_if_some(format!("{root}[vendor_name]"), vendor_name)
            .text_if_some(format!("{root}[driver_version]"), driver_version)
            .text_if_some(format!("{root}[driver_name]"), driver_name);
    }
    if let Some(active_gpu) = metadata.active_gpu.clone() {
        form = form
            .text(
                "sentry[contexts][Active_GPU][Description]",
                "The GPU Zed is running on",
            )
            .text("sentry[contexts][Active_GPU][type]", "gpu")
            .text("sentry[contexts][Active_GPU][name]", active_gpu.device_name)
            .text(
                "sentry[contexts][Active_GPU][driver_version]",
                active_gpu.driver_info,
            )
            .text(
                "sentry[contexts][Active_GPU][driver_name]",
                active_gpu.driver_name,
            )
            .text(
                "sentry[contexts][Active_GPU][is_software_emulated]",
                active_gpu.is_software_emulated.to_string(),
            );
    }

    // TODO: feature-flag-context, and more of device-context like screen resolution, available ram, device model, etc

    let content_type = format!("multipart/form-data; boundary={}", form.boundary());
    let mut body_bytes = Vec::new();
    let mut stream = form
        .into_stream()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        .into_async_read();
    stream.read_to_end(&mut body_bytes).await?;
    let req = Request::builder()
        .method(Method::POST)
        .uri(endpoint)
        .header("Content-Type", content_type)
        .body(AsyncBody::from(body_bytes))?;
    let mut response_text = String::new();
    let mut response = client.http_client().send(req).await?;
    response
        .body_mut()
        .read_to_string(&mut response_text)
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("failed to upload minidump: {response_text}");
    }
    log::info!("Uploaded minidump. event id: {response_text}");
    Ok(())
}

trait FormExt {
    fn text_if_some(
        self,
        label: impl Into<std::borrow::Cow<'static, str>>,
        value: Option<impl Into<std::borrow::Cow<'static, str>>>,
    ) -> Self;
}

impl FormExt for Form {
    fn text_if_some(
        self,
        label: impl Into<std::borrow::Cow<'static, str>>,
        value: Option<impl Into<std::borrow::Cow<'static, str>>>,
    ) -> Self {
        match value {
            Some(value) => self.text(label.into(), value.into()),
            None => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sentry_dsn_valid() {
        let (store_url, key) =
            parse_sentry_dsn("https://abc123@o123456.ingest.sentry.io/7654321").unwrap();
        assert_eq!(
            store_url,
            "https://o123456.ingest.sentry.io/api/7654321/store/"
        );
        assert_eq!(key, "abc123");
    }

    #[test]
    fn test_parse_sentry_dsn_with_port() {
        let (store_url, key) =
            parse_sentry_dsn("https://mykey@sentry.example.com:9000/42").unwrap();
        assert_eq!(store_url, "https://sentry.example.com:9000/api/42/store/");
        assert_eq!(key, "mykey");
    }

    #[test]
    fn test_parse_sentry_dsn_invalid_no_key() {
        assert!(parse_sentry_dsn("https://sentry.io/123").is_none());
    }

    #[test]
    fn test_parse_sentry_dsn_invalid_no_project() {
        assert!(parse_sentry_dsn("https://key@sentry.io/").is_none());
        assert!(parse_sentry_dsn("https://key@sentry.io").is_none());
    }

    #[test]
    fn test_parse_sentry_dsn_invalid_url() {
        assert!(parse_sentry_dsn("not a url at all").is_none());
    }

    fn test_metadata() -> SentryEventMetadata {
        SentryEventMetadata {
            commit_sha: "abc123def".to_owned(),
            zed_version: "1.2.3".to_owned(),
            release_channel: "stable".to_owned(),
            binary: "zed".to_owned(),
            os_name: "macOS".to_owned(),
            os_version: "15.0".to_owned(),
            architecture: "aarch64",
        }
    }

    fn test_event() -> SoftUnreachableEvent {
        SoftUnreachableEvent {
            message: "unexpected variant: Foo".to_owned(),
            backtrace: "  0: some::frame\n  1: another::frame".to_owned(),
            file: "crates/editor/src/editor.rs",
            line: 42,
            timestamp: chrono::DateTime::parse_from_rfc3339("2025-01-15T12:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        }
    }

    #[test]
    fn test_build_sentry_event_json_basic_fields() {
        let event = test_event();
        let metadata = test_metadata();
        let payload = build_sentry_event_json(&event, &metadata, None, None);

        assert_eq!(payload["level"], "error");
        assert_eq!(payload["platform"], "rust");
        assert_eq!(payload["logger"], "soft_unreachable");
        assert_eq!(payload["release"], "abc123def");
        assert_eq!(payload["timestamp"], "2025-01-15T12:30:00Z");

        assert_eq!(payload["tags"]["channel"], "stable");
        assert_eq!(payload["tags"]["version"], "1.2.3");
        assert_eq!(payload["tags"]["binary"], "zed");

        assert_eq!(payload["contexts"]["os"]["name"], "macOS");
        assert_eq!(payload["contexts"]["os"]["version"], "15.0");
        assert_eq!(payload["contexts"]["device"]["arch"], "aarch64");

        assert_eq!(payload["logentry"]["formatted"], "unexpected variant: Foo");

        assert_eq!(payload["extra"]["file"], "crates/editor/src/editor.rs");
        assert_eq!(payload["extra"]["line"], 42);
        assert!(
            payload["extra"]["backtrace"]
                .as_str()
                .unwrap()
                .contains("some::frame")
        );

        let fingerprint = payload["fingerprint"].as_array().unwrap();
        assert_eq!(fingerprint[0], "soft_unreachable");
        assert_eq!(fingerprint[1], "crates/editor/src/editor.rs");
        assert_eq!(fingerprint[2], "42");

        assert!(payload["event_id"].as_str().unwrap().len() == 32);
        assert!(payload.get("user").is_none());
    }

    #[test]
    fn test_build_sentry_event_json_with_user_and_staff() {
        let event = test_event();
        let metadata = test_metadata();
        let payload =
            build_sentry_event_json(&event, &metadata, Some("user-123".to_owned()), Some(true));

        assert_eq!(payload["user"]["id"], "user-123");
        assert_eq!(payload["user"]["is_staff"], "true");
    }

    #[test]
    fn test_build_sentry_event_json_with_user_no_staff() {
        let event = test_event();
        let metadata = test_metadata();
        let payload =
            build_sentry_event_json(&event, &metadata, Some("installation-abc".to_owned()), None);

        assert_eq!(payload["user"]["id"], "installation-abc");
        assert!(payload["user"].get("is_staff").is_none());
    }
}
