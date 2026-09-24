//! `yacs relay update`: update a relay running in Docker on this machine by
//! pulling its image and recreating it with its own compose files, the same
//! `docker compose pull && docker compose up -d` you'd type in deploy/.

use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// A running relay container, from `docker inspect`.
#[derive(Debug, PartialEq)]
pub struct Relay {
    name: String,
    /// As the container was created from it, e.g. `ghcr.io/jonasseifried/yacs:latest`.
    image: String,
    image_id: String,
    pub version: Option<String>,
    compose: Option<Compose>,
}

/// Where the container came from, from the labels docker compose sets.
#[derive(Debug, PartialEq)]
struct Compose {
    project: String,
    working_dir: String,
    files: Vec<String>,
    env_files: Vec<String>,
    service: String,
}

impl Compose {
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("docker");
        cmd.args([
            "compose",
            "-p",
            &self.project,
            "--project-directory",
            &self.working_dir,
        ]);
        for file in &self.files {
            cmd.args(["-f", file]);
        }
        for file in &self.env_files {
            cmd.args(["--env-file", file]);
        }
        cmd.args(args).arg(&self.service);
        cmd
    }
}

pub fn update() -> Result<()> {
    let relays = find()?;
    if relays.is_empty() {
        bail!(
            "no YACS relay is running in Docker on this machine\n(this updates a relay started with docker compose; run it where the relay runs)"
        );
    }
    for relay in relays {
        update_one(&relay)?;
    }
    Ok(())
}

fn update_one(relay: &Relay) -> Result<()> {
    let Some(compose) = &relay.compose else {
        bail!(
            "the relay container {} wasn't started with docker compose; pull {} and recreate it yourself",
            relay.name,
            relay.image
        );
    };
    let version = relay.version.as_deref().unwrap_or("unknown version");
    eprintln!(
        "Relay {} ({version}), from {}",
        relay.name,
        compose.files.join(", ")
    );

    run(compose.command(&["pull"]))?;
    let new_id = docker(&["image", "inspect", "--format", "{{.Id}}", &relay.image])?;
    if new_id.trim() == relay.image_id {
        eprintln!("The relay is up to date.");
        if let Some(tag) = pinned_tag(&relay.image) {
            eprintln!(
                "It's pinned to {tag} (YACS_VERSION in {}/.env); change that to move to a newer release.",
                compose.working_dir
            );
        }
        return Ok(());
    }

    run(compose.command(&["up", "-d"]))?;
    let new_version = image_version(&docker(&[
        "image",
        "inspect",
        "--format",
        "{{json .Config.Labels}}",
        &relay.image,
    ])?);
    eprintln!(
        "Updated the relay: {version} → {}.",
        new_version.as_deref().unwrap_or("the newest image")
    );
    Ok(())
}

/// Running relay containers: any image whose entrypoint is the relay binary,
/// so a fork or a local build counts as well as the published image.
pub fn find() -> Result<Vec<Relay>> {
    let ids = docker(&["ps", "--no-trunc", "--format", "{{.ID}}"])?;
    let ids: Vec<&str> = ids.lines().filter(|l| !l.is_empty()).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["inspect"];
    args.extend(&ids);
    parse_inspect(&docker(&args)?)
}

fn parse_inspect(json: &str) -> Result<Vec<Relay>> {
    let containers: Vec<Value> =
        serde_json::from_str(json).context("couldn't read `docker inspect`'s output")?;
    Ok(containers
        .iter()
        .filter(|c| c["Config"]["Entrypoint"][0] == "/yacs-server")
        .map(|c| {
            let labels = &c["Config"]["Labels"];
            let label = |key: &str| labels[key].as_str().map(str::to_owned);
            let list = |key: &str| {
                label(key)
                    .map(|v| {
                        v.split(',')
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let compose = match (
                label("com.docker.compose.project"),
                label("com.docker.compose.project.working_dir"),
                label("com.docker.compose.service"),
            ) {
                (Some(project), Some(working_dir), Some(service)) => Some(Compose {
                    project,
                    working_dir,
                    files: list("com.docker.compose.project.config_files"),
                    env_files: list("com.docker.compose.project.environment_file"),
                    service,
                }),
                _ => None,
            };
            Relay {
                name: c["Name"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches('/')
                    .to_owned(),
                image: c["Config"]["Image"].as_str().unwrap_or_default().to_owned(),
                image_id: c["Image"].as_str().unwrap_or_default().to_owned(),
                version: image_version(&labels.to_string()),
                compose,
            }
        })
        .collect())
}

/// From the release image's labels, without the tag's `v`.
fn image_version(labels_json: &str) -> Option<String> {
    let labels: Value = serde_json::from_str(labels_json.trim()).ok()?;
    let version = labels["org.opencontainers.image.version"].as_str()?;
    Some(version.trim_start_matches('v').to_owned()).filter(|v| !v.is_empty())
}

/// A tag naming one exact release (`0.2.0`), which pulling never moves.
/// `latest` and `0.2` do move.
fn pinned_tag(image: &str) -> Option<&str> {
    let image = image.split('@').next()?;
    let (_, tag) = image
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))?;
    let parts: Vec<&str> = tag.trim_start_matches('v').split('.').collect();
    (parts.len() == 3 && parts.iter().all(|p| p.parse::<u64>().is_ok())).then_some(tag)
}

fn docker(args: &[&str]) -> Result<String> {
    let output = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(docker_missing)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.to_lowercase().contains("permission denied") {
            bail!(
                "no permission to use Docker; run this with sudo, or add yourself to the docker group"
            );
        }
        bail!("docker {}: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Shows docker's own progress output.
fn run(mut cmd: Command) -> Result<()> {
    let status = cmd.stdin(Stdio::null()).status().map_err(docker_missing)?;
    if !status.success() {
        bail!("docker compose failed ({status})");
    }
    Ok(())
}

fn docker_missing(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!("Docker isn't installed here (there's no `docker` command)")
    } else {
        anyhow::Error::new(e).context("couldn't run docker")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_relays_and_their_compose_project() {
        let json = r#"[
          {
            "Name": "/deploy-yacs-1",
            "Image": "sha256:aaa",
            "Config": {
              "Image": "ghcr.io/jonasseifried/yacs:latest",
              "Entrypoint": ["/yacs-server"],
              "Labels": {
                "com.docker.compose.project": "deploy",
                "com.docker.compose.project.working_dir": "/srv/yacs/deploy",
                "com.docker.compose.project.config_files": "/srv/yacs/deploy/compose.nginx.yaml",
                "com.docker.compose.service": "yacs",
                "org.opencontainers.image.version": "v0.2.1"
              }
            }
          },
          {
            "Name": "/deploy-caddy-1",
            "Image": "sha256:bbb",
            "Config": { "Image": "caddy:2", "Entrypoint": ["caddy"], "Labels": {} }
          },
          {
            "Name": "/by-hand",
            "Image": "sha256:ccc",
            "Config": { "Image": "yacs", "Entrypoint": ["/yacs-server"], "Labels": null }
          }
        ]"#;
        let relays = parse_inspect(json).unwrap();
        assert_eq!(
            relays,
            [
                Relay {
                    name: "deploy-yacs-1".into(),
                    image: "ghcr.io/jonasseifried/yacs:latest".into(),
                    image_id: "sha256:aaa".into(),
                    version: Some("0.2.1".into()),
                    compose: Some(Compose {
                        project: "deploy".into(),
                        working_dir: "/srv/yacs/deploy".into(),
                        files: vec!["/srv/yacs/deploy/compose.nginx.yaml".into()],
                        env_files: vec![],
                        service: "yacs".into(),
                    }),
                },
                Relay {
                    name: "by-hand".into(),
                    image: "yacs".into(),
                    image_id: "sha256:ccc".into(),
                    version: None,
                    compose: None,
                },
            ]
        );

        let cmd = relays[0].compose.as_ref().unwrap().command(&["up", "-d"]);
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "compose",
                "-p",
                "deploy",
                "--project-directory",
                "/srv/yacs/deploy",
                "-f",
                "/srv/yacs/deploy/compose.nginx.yaml",
                "up",
                "-d",
                "yacs"
            ]
        );
    }

    #[test]
    fn knows_pinned_tags() {
        assert_eq!(
            pinned_tag("ghcr.io/jonasseifried/yacs:0.2.0"),
            Some("0.2.0")
        );
        assert_eq!(pinned_tag("localhost:5000/yacs:v1.2.3"), Some("v1.2.3"));
        assert_eq!(pinned_tag("ghcr.io/jonasseifried/yacs:latest"), None);
        assert_eq!(pinned_tag("ghcr.io/jonasseifried/yacs:0.2"), None);
        assert_eq!(pinned_tag("localhost:5000/yacs"), None);
        assert_eq!(pinned_tag("yacs@sha256:abc"), None);
    }
}
