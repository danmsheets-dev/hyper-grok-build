use super::*;

#[tokio::test]
#[serial_test::serial]
async fn rc4_clear_default_removes_the_persisted_key() {
    let home = tempfile::tempdir().unwrap();
    let _env = xai_grok_test_support::EnvGuard::set("GROK_HOME", home.path());
    let _campaigns = xai_grok_test_support::EnvGuard::set("GROK_CAMPAIGNS_OVERRIDE", "[]");
    let path = home.path().join("config.toml");
    std::fs::write(
        &path,
        "[models]\ndefault = \"grok-old\"\nweb_search = \"keep-search\"\nfuture_field = \"keep-me\"\n",
    )
    .unwrap();

    super::super::settings_writes::set_default_model(String::new())
        .await
        .unwrap();

    let saved: TomlValue = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let models = saved.get("models").unwrap().as_table().unwrap();
    assert!(
        !models.contains_key("default"),
        "clearing must delete the disk key"
    );
    assert_eq!(models["web_search"].as_str(), Some("keep-search"));
    assert_eq!(models["future_field"].as_str(), Some("keep-me"));
    assert!(load_config_from_toml(&saved).models.default.is_none());
}

fn wait_for_file(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

struct WriterChild {
    child: Option<std::process::Child>,
    release: std::path::PathBuf,
}

impl WriterChild {
    fn start(home: &std::path::Path, role: &str) -> Self {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "util::config::persist::rc4_tests::rc4_config_writer_child",
                "--nocapture",
            ])
            .env("GROK_HOME", home)
            .env("RC4_CONFIG_TEST_HOME", home)
            .env("RC4_CONFIG_TEST_ROLE", role)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            child: Some(child),
            release: home.join("release-theme"),
        }
    }

    fn finish(mut self) {
        let output = self.child.take().unwrap().wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for WriterChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = std::fs::write(&self.release, b"continue");
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

async fn mutate_other_config(role: &str, home: &std::path::Path) -> Result<()> {
    let path = home.join("config.toml");
    match role {
        "disabled-tools" => {
            super::super::mcp::save_mcp_disabled_tools("demo", &["tool-a".into()]).await
        }
        "upsert" => {
            let config = toml::from_str("command = \"rc4-helper\"").unwrap();
            super::super::mcp::save_mcp_server_config_at(&path, "added", &config).await
        }
        "delete" => super::super::mcp::delete_mcp_server_config_at(&path, "demo")
            .await
            .map(|_| ()),
        "marketplace" => {
            crate::extensions::marketplace::ensure_official_marketplace_source(home);
            Ok(())
        }
        _ => panic!("unknown mutation {role}"),
    }
}

#[test]
fn rc4_config_writer_child() {
    let Some(home) = std::env::var_os("RC4_CONFIG_TEST_HOME") else {
        return;
    };
    let home = std::path::PathBuf::from(home);
    assert_eq!(user_config_path(), home.join("config.toml"));
    let role = std::env::var("RC4_CONFIG_TEST_ROLE").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        if role == "theme" || role == "model" {
            update_config(|cfg| {
                if role == "theme" {
                    std::fs::write(home.join("theme-read"), b"ready").unwrap();
                    wait_for_file(&home.join("release-theme"));
                    cfg.ui.theme = Some("tokyonight".to_string());
                } else {
                    cfg.models.default = Some("grok-new".to_string());
                }
            })
            .await
            .unwrap();
        } else {
            std::fs::write(home.join("operation-started"), b"ready").unwrap();
            mutate_other_config(&role, &home).await.unwrap();
            std::fs::write(home.join("operation-done"), b"done").unwrap();
        }
    });
}

#[test]
#[serial_test::serial]
fn rc4_independent_config_writers_hold_one_transaction_lock() {
    use fs2::FileExt as _;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    std::fs::write(&path, "[models]\ndefault = \"grok-old\"\n").unwrap();
    let theme = WriterChild::start(home.path(), "theme");
    wait_for_file(&home.path().join("theme-read"));
    let probe = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("toml.lock"))
        .unwrap();
    let guarded = match probe.try_lock_exclusive() {
        Ok(()) => {
            fs2::FileExt::unlock(&probe).unwrap();
            false
        }
        Err(error) => {
            assert!(
                xai_grok_workspace::util::is_lock_contended(&error),
                "unexpected lock error: {error}"
            );
            true
        }
    };
    let model = WriterChild::start(home.path(), "model");
    std::fs::write(home.path().join("release-theme"), b"continue").unwrap();
    theme.finish();
    model.finish();
    let saved: TomlValue = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(
        guarded,
        "the read-modify-write must hold an interprocess file lock"
    );
    assert_eq!(saved["models"]["default"].as_str(), Some("grok-new"));
    assert_eq!(saved["ui"]["theme"].as_str(), Some("tokyonight"));
}

#[tokio::test]
#[serial_test::serial]
async fn rc4_mcp_writers_preserve_malformed_config() {
    let home = tempfile::tempdir().unwrap();
    let _env = xai_grok_test_support::EnvGuard::set("GROK_HOME", home.path());
    let path = home.path().join("config.toml");
    let original = "synthetic_private_value = \"do-not-echo-fixture\"\n[broken\n";
    for role in ["disabled-tools", "upsert", "delete"] {
        std::fs::write(&path, original).unwrap();
        let result = mutate_other_config(role, home.path()).await;
        assert!(result.is_err(), "{role} must reject malformed config");
        assert!(
            !result
                .unwrap_err()
                .to_string()
                .contains("do-not-echo-fixture")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}

#[test]
#[serial_test::serial]
fn rc4_other_process_writers_respect_the_settings_transaction() {
    use fs2::FileExt as _;
    for role in ["disabled-tools", "upsert", "delete", "marketplace"] {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let original =
            "[models]\ndefault = \"grok-old\"\n[mcp_servers.demo]\ncommand = \"rc4-original\"\n";
        std::fs::write(&path, original).unwrap();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("toml.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let child = WriterChild::start(home.path(), role);
        wait_for_file(&home.path().join("operation-started"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !home.path().join("operation-done").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let bypassed = home.path().join("operation-done").exists();
        std::fs::write(&path, original.replace("grok-old", "grok-new")).unwrap();
        fs2::FileExt::unlock(&lock).unwrap();
        child.finish();
        assert!(
            !bypassed,
            "{role} wrote while another process held the config transaction"
        );
        let saved: TomlValue = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["models"]["default"].as_str(), Some("grok-new"));
        match role {
            "disabled-tools" => assert_eq!(
                saved["disabled_mcp_tools"]["demo"][0].as_str(),
                Some("tool-a")
            ),
            "upsert" => assert_eq!(
                saved["mcp_servers"]["added"]["command"].as_str(),
                Some("rc4-helper")
            ),
            "delete" => assert!(
                saved
                    .get("mcp_servers")
                    .and_then(|servers| servers.get("demo"))
                    .is_none()
            ),
            "marketplace" => assert_eq!(
                saved["marketplace"]["official_marketplace_auto_installed"].as_bool(),
                Some(true)
            ),
            _ => unreachable!(),
        }
    }
}
