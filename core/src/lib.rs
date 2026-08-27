//! Shared library for Capture to Search: paths, config, IPC, autostart,
//! screen capture backends, and the Google Lens upload.
//!
//! Deliberately free of any GUI toolkit dependency so the daemon (which owns
//! the tray, capture, upload, and browser launch) builds and runs on hosts with
//! no GTK at all. The GUI crate is an optional front end over this.

pub mod autostart;
pub mod capture;
pub mod config;
pub mod ipc;
pub mod lens;
pub mod paths;

pub use config::Config;

/// Reverse-DNS application ID: tray id, GTK application id, icon name, and the
/// basename of the `.desktop` files.
pub const APP_ID: &str = "io.github.dipakmdhrm.CaptureToSearch";

/// Human-readable application name, shown in the tray tooltip and About dialog.
pub const APP_NAME: &str = "Capture to Search";

/// Short name used for config/runtime directories and the autostart entry.
pub const APP_SLUG: &str = "capture-to-search";

/// Install the process-wide rustls crypto provider.
///
/// We build reqwest with `rustls-no-provider`, so exactly one provider must be
/// installed before the first TLS handshake. Idempotent: a second call is a
/// no-op rather than an error, so both the daemon and a one-shot `capture` run
/// can call it unconditionally at startup.
pub fn install_default_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// The `--version` blurb shared by every entry point.
pub fn version_blurb(bin: &str) -> String {
    format!("{bin} {} ({APP_NAME})", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod packaging_tests {
    use super::*;

    /// Repo root, from this crate's manifest directory.
    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn read(rel: &str) -> String {
        let path = root().join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing packaging file {}: {e}", path.display()))
    }

    /// Every file that has to name the binaries or the app id.
    const PACKAGING_FILES: &[&str] = &[
        "packaging/deb/control.template",
        "packaging/deb/prerm",
        "packaging/deb/postrm",
        "packaging/rpm/capture-to-search.spec",
        "packaging/arch/PKGBUILD",
        "packaging/arch/capture-to-search.install",
        "packaging/build-local.sh",
        "packaging/stage-tree.sh",
        "packaging/source-tarball.sh",
        "packaging/next-version.sh",
        "install.sh",
        "uninstall.sh",
        ".github/workflows/ci.yml",
        ".github/workflows/build-packages.yml",
        ".github/workflows/release.yml",
        ".github/workflows/auto-release.yml",
        "docs/RELEASING.md",
    ];

    #[test]
    fn packaging_installs_the_binaries_we_actually_build() {
        // Renaming a binary without updating packaging produces a package that
        // installs cleanly and then does nothing - the desktop entry points at
        // a path that does not exist. Nothing else catches this.
        let daemon = format!("{APP_SLUG}d");
        for file in [
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
        ] {
            let text = read(file);
            assert!(text.contains(&daemon), "{file} does not install {daemon}");
            assert!(
                text.contains(&format!("{APP_SLUG}-gui")),
                "{file} does not install {APP_SLUG}-gui"
            );
        }
    }

    #[test]
    fn packaging_agrees_on_the_app_id() {
        // The app id is the icon name, the desktop file basename, and the tray
        // id. A package that installs icons under a different id shows a
        // missing-icon placeholder.
        for file in [
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
            "packaging/stage-tree.sh",
            "install.sh",
            "uninstall.sh",
        ] {
            assert!(
                read(file).contains(APP_ID),
                "{file} does not reference the app id {APP_ID}"
            );
        }
    }

    #[test]
    fn package_names_match_the_slug() {
        assert!(
            read("packaging/deb/control.template").contains(&format!("Package: {APP_SLUG}")),
            "deb package name should be {APP_SLUG}"
        );
        assert!(
            read("packaging/rpm/capture-to-search.spec")
                .contains(&format!("Name:           {APP_SLUG}")),
            "rpm package name should be {APP_SLUG}"
        );
        assert!(
            read("packaging/arch/PKGBUILD").contains(&format!("pkgname={APP_SLUG}")),
            "arch package name should be {APP_SLUG}"
        );
    }

    #[test]
    fn versions_are_templated_not_hardcoded() {
        // A hardcoded version silently ships the wrong number forever, since
        // nothing rebuilds these files.
        for file in [
            "packaging/deb/control.template",
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
        ] {
            let text = read(file);
            assert!(
                text.contains("@VERSION@"),
                "{file} should template the version, not hardcode it"
            );
            assert!(
                !text.contains(env!("CARGO_PKG_VERSION")),
                "{file} hardcodes the current version instead of using @VERSION@"
            );
        }
    }

    #[test]
    fn removal_cleans_up_the_autostart_entry() {
        // The app writes ~/.config/autostart per user, so packaging is the only
        // thing that can remove it. Leaving it behind means an uninstalled app
        // still tries to start at login.
        let entry = "autostart/capture-to-search.desktop";
        for file in [
            "packaging/deb/postrm",
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/capture-to-search.install",
            "uninstall.sh",
        ] {
            assert!(
                read(file).contains(entry),
                "{file} should remove the autostart entry on uninstall"
            );
        }
    }

    #[test]
    fn upgrades_do_not_kill_the_running_daemon() {
        // The daemon re-execs onto a replaced binary itself (self_update), so
        // packaging must only stop it on a real removal. Killing on upgrade
        // would make the tray icon vanish until the next login.
        let prerm = read("packaging/deb/prerm");
        assert!(
            prerm.contains(r#"if [ "$1" = "remove" ]"#),
            "deb prerm must only stop the daemon on removal, not upgrade"
        );
        let arch = read("packaging/arch/capture-to-search.install");
        let post_upgrade = arch
            .split("post_upgrade()")
            .nth(1)
            .expect("PKGBUILD install file needs a post_upgrade hook");
        let body = &post_upgrade[..post_upgrade.find("\n}").unwrap_or(post_upgrade.len())];
        assert!(
            !body.contains("pkill"),
            "arch post_upgrade must not kill the daemon"
        );
    }

    #[test]
    fn the_symbolic_icon_is_packaged_everywhere() {
        // The one asset whose filename is not `<appid>.<ext>`, so every glob
        // written the obvious way misses it. rpmbuild turns that into a hard
        // "installed but unpackaged" error; the other formats just ship without
        // it and the tray icon stops adapting to light and dark panels.
        for file in [
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
            "packaging/stage-tree.sh",
            "install.sh",
        ] {
            assert!(
                read(file).contains("-symbolic.svg"),
                "{file} does not handle the symbolic tray icon"
            );
        }
    }

    #[test]
    fn ci_runs_the_checks_the_contributor_guide_mandates() {
        // CLAUDE.md tells contributors to run these three before opening a PR.
        // If CI silently stops running one, the documented process and the
        // enforced process drift apart, and the gap only shows up as a
        // regression that everyone assumed was covered.
        let ci = read(".github/workflows/ci.yml");
        for check in [
            "cargo fmt --all -- --check",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo test --workspace",
        ] {
            assert!(ci.contains(check), "CI does not run `{check}`");
        }
    }

    #[test]
    fn every_package_is_built_on_every_pull_request() {
        // A tag build should be a repeat of something already validated, not
        // the first time anyone tried to build a package. CI reaches the
        // packages through the same reusable workflow the release uses.
        let ci = read(".github/workflows/ci.yml");
        assert!(
            ci.contains("uses: ./.github/workflows/build-packages.yml"),
            "CI does not build the packages on pull requests"
        );
        let release = read(".github/workflows/release.yml");
        assert!(
            release.contains("uses: ./.github/workflows/build-packages.yml"),
            "the release must reuse the workflow CI validated, not its own copy"
        );

        let build = read(".github/workflows/build-packages.yml");
        for job in ["build-deb:", "build-rpm:", "build-arch:"] {
            assert!(build.contains(job), "build-packages.yml is missing {job}");
        }
    }

    #[test]
    fn ci_and_local_builds_share_their_packaging_scripts() {
        // The one bug that has already happened twice here: a file staged by
        // one build path and forgotten by the other. Both must go through the
        // shared scripts rather than keeping private copies of the layout.
        let build = read(".github/workflows/build-packages.yml");
        let local = read("packaging/build-local.sh");
        for script in ["stage-tree.sh", "source-tarball.sh"] {
            assert!(build.contains(script), "CI does not use packaging/{script}");
            assert!(
                local.contains(script),
                "build-local.sh does not use packaging/{script}"
            );
        }
    }

    #[test]
    fn the_deb_registers_and_unregisters_the_apt_repository() {
        // Publishing a repository nothing subscribes to would be pointless, and
        // leaving the source list behind after removal makes `apt update` keep
        // fetching a repository for software that is gone.
        let postinst = read("packaging/deb/postinst");
        assert!(
            postinst.contains("sources.list.d/capture-to-search.list"),
            "postinst should register the apt repository"
        );
        assert!(
            postinst.contains(r#"if [ -z "$2" ]"#),
            "the repository should only be registered on a fresh install, not on upgrade"
        );

        let postrm = read("packaging/deb/postrm");
        for f in [
            "sources.list.d/capture-to-search.list",
            "keyrings/capture-to-search.gpg",
        ] {
            assert!(postrm.contains(f), "postrm should remove {f}");
        }
    }

    #[test]
    fn the_deb_depends_on_what_its_postinst_uses() {
        // postinst fetches the repository key with curl and stores it with gnupg.
        // Without these declared, a minimal system fails silently at install
        // time and the package never auto-updates.
        let control = read("packaging/deb/control.template");
        let depends = control
            .lines()
            .find(|l| l.starts_with("Depends:"))
            .expect("control template has no Depends line");
        let postinst = read("packaging/deb/postinst");
        for tool in ["curl", "gnupg"] {
            let used = postinst.contains(match tool {
                "gnupg" => "keyrings",
                other => other,
            });
            if used {
                assert!(
                    depends.contains(tool),
                    "Depends is missing {tool}, used by postinst"
                );
            }
        }
    }

    #[test]
    fn publishing_the_apt_repository_never_blocks_a_release() {
        // The signing key is optional setup. Before it exists the job must skip
        // cleanly, and it must never gate the GitHub Release - otherwise a
        // repository misconfiguration loses the packages entirely.
        let release = read(".github/workflows/release.yml");
        assert!(
            release.contains("APT_SIGNING_KEY"),
            "release.yml does not publish the apt repository"
        );
        assert!(
            release.contains("enabled=false"),
            "the publishing job must skip cleanly when no signing key is set"
        );

        // The `release` job must not depend on `pages`.
        let release_job = release
            .split("\n  release:")
            .nth(1)
            .expect("release.yml has no release job");
        let needs = release_job
            .lines()
            .find(|l| l.trim_start().starts_with("needs:"))
            .unwrap_or("");
        assert!(
            !needs.contains("pages"),
            "the GitHub Release must not depend on apt publishing, got `{needs}`"
        );
    }

    /// Parse "1.2.3" for comparison.
    fn semver(v: &str) -> (u64, u64, u64) {
        let mut it = v.trim().split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    }

    fn manifest_version() -> String {
        let toml = read("Cargo.toml");
        let after = toml
            .split("[workspace.package]")
            .nth(1)
            .expect("no [workspace.package] section");
        after
            .lines()
            .find_map(|l| l.strip_prefix("version = \""))
            .and_then(|v| v.split('\"').next())
            .expect("no version in [workspace.package]")
            .to_string()
    }

    #[test]
    fn the_next_version_never_goes_backwards() {
        // The release version is computed from git tags, but the manifest and
        // changelog can already claim a higher one - as they did here, with
        // 0.1.0 declared before any tag existed. A tag-only base would have
        // released 0.0.1, rewriting the manifest backwards and stamping a 0.0.1
        // changelog section above the 0.1.0 one.
        let root = root();
        let manifest = semver(&manifest_version());

        for bump in ["patch", "minor", "major"] {
            let out = std::process::Command::new(root.join("packaging/next-version.sh"))
                .arg(bump)
                .arg(&root)
                .output()
                .expect("next-version.sh should run");
            assert!(
                out.status.success(),
                "next-version.sh {bump} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let next = semver(&String::from_utf8_lossy(&out.stdout));
            assert!(
                next > manifest,
                "{bump} produced {next:?}, which is not ahead of the manifest {manifest:?}"
            );
        }
    }

    #[test]
    fn an_unknown_bump_is_rejected() {
        // A typo in a release label must not quietly release something.
        let root = root();
        let out = std::process::Command::new(root.join("packaging/next-version.sh"))
            .arg("pathc")
            .arg(&root)
            .output()
            .expect("next-version.sh should run");
        assert!(!out.status.success(), "an unknown bump should fail");
    }

    #[test]
    fn the_release_workflow_uses_the_shared_version_rule() {
        // Reinlining the calculation into YAML would put it beyond the reach of
        // the test above.
        let wf = read(".github/workflows/auto-release.yml");
        assert!(
            wf.contains("next-version.sh"),
            "auto-release.yml should delegate the version calculation"
        );
        assert!(
            !wf.contains("sort=-v:refname"),
            "auto-release.yml should not compute versions itself any more"
        );
    }

    #[test]
    fn every_packaging_file_is_present() {
        for file in PACKAGING_FILES {
            let path = root().join(file);
            assert!(path.is_file(), "missing {}", path.display());
        }
    }
}
