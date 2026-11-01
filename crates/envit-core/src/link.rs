//! Linking (PRD §12). M1: symlinks (macOS/Linux) with atomic flips.
//! Windows junctions and the reflink/hardlink/copy fallback matrix land
//! in M2.

use std::path::Path;

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    Created,
    /// Link existed but pointed elsewhere; atomically flipped.
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkMode {
    #[default]
    Symlink,
    /// Escape hatch for filesystems where symlinks don't work (network
    /// shares, restrictive container mounts). Loses byte-sharing.
    Copy,
}

/// Point `link` at the store checkout `target` using `mode`. `commit` keys
/// copy-mode freshness (a `<link>.commit` marker file).
pub fn materialize_link(
    link: &Path,
    target: &Path,
    mode: LinkMode,
    commit: &str,
) -> Result<LinkOutcome, Error> {
    match mode {
        LinkMode::Symlink => link_dir(link, target),
        LinkMode::Copy => copy_dir(link, target, commit),
    }
}

/// Materialize a native agent definition file. Symlinks may replace only
/// files already pointing into envit's store; copies require our commit marker.
pub fn materialize_file_link(
    link: &Path,
    target: &Path,
    mode: LinkMode,
    key: &str,
    store_root: &Path,
) -> Result<LinkOutcome, Error> {
    match mode {
        LinkMode::Symlink => link_file(link, target, store_root),
        LinkMode::Copy => materialize_file_copy(link, target, key, store_root),
    }
}

/// Copy generated agent content into place with a marker keyed to its source.
/// Generated files remain copies even when the project uses symlink mode.
pub fn materialize_file_contents(
    link: &Path,
    content: &[u8],
    key: &str,
    store_root: &Path,
) -> Result<LinkOutcome, Error> {
    let marker = marker_path(link);
    if std::fs::symlink_metadata(link).is_ok_and(|meta| meta.is_file())
        && std::fs::read_to_string(&marker).ok().as_deref() == Some(key)
    {
        return Ok(LinkOutcome::Unchanged);
    }
    if file_exists(link) && !managed_file(link, store_root) {
        return Err(Error::LinkObstructed(link.to_path_buf()));
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_name(link);
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, content)?;
    let mut perms = std::fs::metadata(&tmp)?.permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&tmp, perms)?;
    let existed = file_exists(link);
    if existed {
        remove_managed_file(link, store_root)?;
    }
    std::fs::rename(&tmp, link)?;
    std::fs::write(&marker, key)?;
    Ok(if existed {
        LinkOutcome::Updated
    } else {
        LinkOutcome::Created
    })
}

/// A marker sidecar identifies files envit materialized in copy mode or generated.
pub(crate) fn file_marker_path(link: &Path) -> std::path::PathBuf {
    marker_path(link)
}

/// True when the last materialize_copy for `link` wrote fresh content for
/// `key` (i.e. the marker now equals `key` and content was just copied).
/// Freshness is recorded by materialize_copy in a side channel: the marker
/// write itself. Callers patch files only on fresh copies.
pub fn copy_was_fresh(link: &Path, key: &str) -> bool {
    // materialize_copy sets the marker only after a fresh copy; an
    // unchanged marker means the previous (already patched) copy stands.
    std::fs::read_to_string(fresh_flag_path(link))
        .ok()
        .as_deref()
        == Some(key)
}

fn fresh_flag_path(link: &Path) -> std::path::PathBuf {
    let mut name = link.file_name().unwrap_or_default().to_os_string();
    name.push(".fresh");
    link.with_file_name(name)
}

/// Copy `target` to `link` with marker `key`. No-op when the marker
/// already equals `key`. Leaves a `.fresh` flag when it actually copied,
/// so the caller knows to apply patches exactly once.
pub fn materialize_copy(link: &Path, target: &Path, key: &str) -> Result<(), Error> {
    let flag = fresh_flag_path(link);
    let outcome = copy_dir(link, target, key)?;
    if outcome == LinkOutcome::Unchanged {
        let _ = std::fs::remove_file(&flag);
    } else {
        std::fs::write(&flag, key)?;
    }
    Ok(())
}

fn marker_path(link: &Path) -> std::path::PathBuf {
    let mut name = link.file_name().unwrap_or_default().to_os_string();
    name.push(".commit");
    link.with_file_name(name)
}

fn file_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn managed_file(path: &Path, store_root: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return std::fs::read_link(path).is_ok_and(|target| target.starts_with(store_root));
    }
    meta.is_file() && marker_path(path).is_file()
}

fn remove_managed_file(path: &Path, store_root: &Path) -> Result<(), Error> {
    if !managed_file(path, store_root) {
        return Err(Error::LinkObstructed(path.to_path_buf()));
    }
    if file_exists(path) {
        std::fs::remove_file(path)?;
    }
    let _ = std::fs::remove_file(marker_path(path));
    Ok(())
}

fn materialize_file_copy(
    link: &Path,
    target: &Path,
    key: &str,
    store_root: &Path,
) -> Result<LinkOutcome, Error> {
    let content = std::fs::read(target)?;
    materialize_file_contents(link, &content, key, store_root)
}

fn link_file(link: &Path, target: &Path, store_root: &Path) -> Result<LinkOutcome, Error> {
    if let Ok(current) = std::fs::read_link(link) {
        if current == target {
            return Ok(LinkOutcome::Unchanged);
        }
        if !managed_file(link, store_root) {
            return Err(Error::LinkObstructed(link.to_path_buf()));
        }
        let tmp = tmp_name(link);
        let _ = std::fs::remove_file(&tmp);
        symlink_file(target, &tmp)?;
        std::fs::rename(&tmp, link)?;
        let _ = std::fs::remove_file(marker_path(link));
        return Ok(LinkOutcome::Updated);
    }
    if file_exists(link) {
        if !managed_file(link, store_root) {
            return Err(Error::LinkObstructed(link.to_path_buf()));
        }
        std::fs::remove_file(link)?;
        let _ = std::fs::remove_file(marker_path(link));
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    symlink_file(target, link)?;
    Ok(LinkOutcome::Created)
}

fn copy_dir(link: &Path, target: &Path, commit: &str) -> Result<LinkOutcome, Error> {
    let marker = marker_path(link);
    if link.is_dir() {
        if std::fs::read_to_string(&marker).ok().as_deref() == Some(commit) {
            return Ok(LinkOutcome::Unchanged);
        }
        if std::fs::read_link(link).is_ok() || !marker.exists() {
            // A symlink (mode switch) is fine to replace; an unmarked real
            // dir is not ours to delete.
            if std::fs::read_link(link).is_err() {
                return Err(Error::LinkObstructed(link.to_path_buf()));
            }
        }
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_name(link);
    let _ = std::fs::remove_dir_all(&tmp);
    copy_tree(target, &tmp)?;
    let existed = link.exists() || std::fs::read_link(link).is_ok();
    if existed {
        if std::fs::read_link(link).is_ok() {
            std::fs::remove_file(link)?;
        } else {
            std::fs::remove_dir_all(link)?;
        }
    }
    std::fs::rename(&tmp, link)?;
    std::fs::write(&marker, commit)?;
    Ok(if existed {
        LinkOutcome::Updated
    } else {
        LinkOutcome::Created
    })
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Make `link` point at `target`. Existing correct links are left alone;
/// a stale link is replaced atomically (build at temp name, rename over)
/// so a concurrent reader never sees a missing or half-made link.
pub fn link_dir(link: &Path, target: &Path) -> Result<LinkOutcome, Error> {
    if let Ok(current) = std::fs::read_link(link) {
        if current == target {
            return Ok(LinkOutcome::Unchanged);
        }
        let tmp = tmp_name(link);
        let _ = std::fs::remove_file(&tmp);
        symlink_dir(target, &tmp)?;
        std::fs::rename(&tmp, link)?;
        return Ok(LinkOutcome::Updated);
    }
    if link.exists() {
        // A marker-backed dir is a managed copy (copy mode or a patched
        // skill): ours to replace on a mode switch. Anything else is the
        // user's; refuse to destroy it.
        let marker = marker_path(link);
        if marker.exists() {
            std::fs::remove_dir_all(link)?;
            let _ = std::fs::remove_file(&marker);
            let _ = std::fs::remove_file(fresh_flag_path(link));
        } else {
            return Err(Error::LinkObstructed(link.to_path_buf()));
        }
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    symlink_dir(target, link)?;
    Ok(LinkOutcome::Created)
}

fn tmp_name(link: &Path) -> std::path::PathBuf {
    let mut name = link.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp-link");
    link.with_file_name(name)
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn symlink_dir(_target: &Path, _link: &Path) -> Result<(), Error> {
    // M2: directory junctions (no admin required).
    Err(Error::Git("Windows linking lands in M2".to_string()))
}

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) -> Result<(), Error> {
    std::os::windows::fs::symlink_file(target, link)?;
    Ok(())
}
