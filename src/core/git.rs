use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use crate::core::error::{ArchivaError, Result};
use crate::core::fs::read_text_file_with_limit;
use crate::core::hash::sha256;
use crate::core::paths::RelativePath;

const GIT_OUTPUT_MAX_BYTES: usize = 10 * 1024 * 1024;
const GIT_OBJECT_STORAGE_MAX_BYTES: usize = GIT_OUTPUT_MAX_BYTES + 1024 * 1024;
const GIT_MARKER_MAX_BYTES: usize = 64 * 1024;
const GIT_SHA1_HEX_BYTES: usize = 40;
const GIT_SHA1_BYTES: usize = 20;
const GIT_SHA256_HEX_BYTES: usize = 64;
const GIT_SHA256_BYTES: usize = 32;
#[cfg(test)]
const GIT_OID_BYTES: usize = GIT_SHA1_BYTES;
const GIT_PACK_SIGNATURE: &[u8; 4] = b"PACK";
const GIT_PACK_INDEX_MAGIC: &[u8; 4] = &[0xff, b't', b'O', b'c'];
const GIT_PACK_INDEX_VERSION: u32 = 2;
#[cfg(test)]
const GIT_PACK_TRAILER_BYTES: u64 = GIT_SHA1_BYTES as u64;
const GIT_PACK_INDEX_HEADER_BYTES: u64 = 8;
const GIT_PACK_INDEX_FANOUT_BYTES: u64 = 256 * 4;
const GIT_PACK_INDEX_CRC_BYTES: u64 = 4;
const GIT_PACK_INDEX_OFFSET_BYTES: u64 = 4;
const GIT_PACK_INDEX_LARGE_OFFSET_BYTES: u64 = 8;
const GIT_PACK_DELTA_MAX_DEPTH: usize = 32;
const GIT_ALTERNATES_MAX_DEPTH: usize = 8;
const GIT_SYMBOLIC_REF_MAX_DEPTH: usize = 8;

#[derive(Debug)]
struct GitObject {
    kind: String,
    data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitObjectFormat {
    Sha1,
    Sha256,
}

impl GitObjectFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    fn raw_len(self) -> usize {
        match self {
            Self::Sha1 => GIT_SHA1_BYTES,
            Self::Sha256 => GIT_SHA256_BYTES,
        }
    }

    fn raw_len_u64(self) -> u64 {
        self.raw_len() as u64
    }

    fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => GIT_SHA1_HEX_BYTES,
            Self::Sha256 => GIT_SHA256_HEX_BYTES,
        }
    }

    fn pack_index_v1_entry_bytes(self) -> u64 {
        GIT_PACK_INDEX_OFFSET_BYTES + self.raw_len_u64()
    }

    fn pack_index_trailer_bytes(self) -> u64 {
        self.raw_len_u64() * 2
    }
}

#[derive(Default)]
struct GitObjectReadContext {
    validated_index_checksums: HashSet<PathBuf>,
    validated_index_layouts: HashSet<PathBuf>,
    validated_index_pack_pairs: HashSet<PathBuf>,
    pack_index_offsets: HashMap<PathBuf, Vec<u64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PackIndexFormat {
    V1,
    V2,
}

#[derive(Clone, Copy)]
struct PackIndexLayout {
    format: PackIndexFormat,
    object_format: GitObjectFormat,
    object_count: u32,
    names_offset: u64,
    offset_table_offset: u64,
    large_offset_table_offset: Option<u64>,
}

#[derive(Clone)]
struct PackObjectLocation {
    idx_path: PathBuf,
    pack_path: PathBuf,
    offset: u64,
    next_offset: u64,
    pack_data_end: u64,
    index_layout: PackIndexLayout,
}

#[derive(Debug)]
struct Huffman {
    entries: Vec<HuffmanEntry>,
    max_len: u8,
}

#[derive(Debug)]
struct HuffmanEntry {
    code: u16,
    len: u8,
    symbol: u16,
}

struct BitReader<'a> {
    bytes: &'a [u8],
    byte_index: usize,
    bit_index: u8,
}

pub fn find_git_root(start_dir: &Path) -> Result<Option<PathBuf>> {
    let mut dir = start_dir.canonicalize().map_err(|source| {
        ArchivaError::io(
            Some(start_dir.to_path_buf()),
            "resolve git search root",
            source,
        )
    })?;

    loop {
        if has_git_work_tree_marker(&dir) {
            return Ok(Some(dir));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

pub fn read_git_head_file(project_root: &Path, file: &RelativePath) -> Result<String> {
    read_git_head_file_native(project_root, file)
}

fn read_git_head_file_native(project_root: &Path, file: &RelativePath) -> Result<String> {
    let git_root = find_git_root(project_root)?.ok_or_else(|| ArchivaError::Git {
        message: "Not a git repository".to_string(),
    })?;
    let project_root = canonical_project_root(project_root)?;
    let relative_to_git = project_file_to_git_relative(&project_root, &git_root, file)?;
    let git_dir = git_dir_for_work_tree(&git_root)?;
    let object_format = git_object_format(&git_dir)?;
    let head_oid = resolve_head_oid(&git_dir, object_format)?;
    let mut context = GitObjectReadContext::default();
    let commit = read_git_object_with_context(&git_dir, &head_oid, object_format, &mut context)?;
    if commit.kind != "commit" {
        return Err(git_error(format!("HEAD object {head_oid} is not a commit")));
    }
    let tree_oid = commit_tree_oid(&commit.data, object_format)?;
    let blob_oid = tree_blob_oid(
        &git_dir,
        &tree_oid,
        &relative_to_git,
        object_format,
        &mut context,
    )?;
    let blob = read_git_object_with_context(&git_dir, &blob_oid, object_format, &mut context)?;
    if blob.kind != "blob" {
        return Err(git_error(format!(
            "HEAD:{relative_to_git} resolved to a {} object",
            blob.kind
        )));
    }
    String::from_utf8(blob.data).map_err(|source| ArchivaError::Git {
        message: format!("HEAD:{relative_to_git} returned non-UTF-8 content: {source}"),
    })
}

fn git_dir_for_work_tree(git_root: &Path) -> Result<PathBuf> {
    let marker = git_root.join(".git");
    let metadata = fs::metadata(&marker)
        .map_err(|source| ArchivaError::io(Some(marker.clone()), "read git marker", source))?;
    if metadata.is_dir() {
        return Ok(marker);
    }
    if !metadata.is_file() {
        return Err(git_error("Invalid .git marker"));
    }
    let content = read_text_file_with_limit(&marker, GIT_MARKER_MAX_BYTES, "read git marker")?;
    let Some(line) = content.lines().next() else {
        return Err(git_error("Invalid .git marker"));
    };
    let Some(path) = line.trim().strip_prefix("gitdir:") else {
        return Err(git_error("Invalid .git marker"));
    };
    let path = PathBuf::from(path.trim());
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(git_root.join(path))
    }
}

fn git_object_format(git_dir: &Path) -> Result<GitObjectFormat> {
    let mut dirs = vec![git_dir.to_path_buf()];
    if let Some(common_dir) = common_git_dir(git_dir)? {
        if common_dir != git_dir {
            dirs.push(common_dir);
        }
    }
    let mut detected: Option<GitObjectFormat> = None;
    for dir in dirs {
        if let Some(format) = git_config_object_format(&dir)? {
            let parsed = parse_git_object_format(&format)?;
            if let Some(existing) = detected {
                if existing != parsed {
                    return Err(git_error(format!(
                        "Conflicting git object formats {} and {}",
                        existing.name(),
                        parsed.name()
                    )));
                }
            } else {
                detected = Some(parsed);
            }
        }
    }
    Ok(detected.unwrap_or(GitObjectFormat::Sha1))
}

fn parse_git_object_format(format: &str) -> Result<GitObjectFormat> {
    if format.eq_ignore_ascii_case("sha1") {
        return Ok(GitObjectFormat::Sha1);
    }
    if format.eq_ignore_ascii_case("sha256") {
        return Ok(GitObjectFormat::Sha256);
    }
    Err(git_error(format!(
        "Git object format {format:?} is unsupported; only sha1 and sha256 repositories are supported"
    )))
}

fn git_config_object_format(git_dir: &Path) -> Result<Option<String>> {
    let config_path = git_dir.join("config");
    let content =
        match read_text_file_with_limit(&config_path, GIT_MARKER_MAX_BYTES, "read git config") {
            Ok(content) => content,
            Err(ArchivaError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
    let mut in_extensions = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let section = line[1..line.len() - 1].trim();
            in_extensions = section
                .split_whitespace()
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("extensions"));
            continue;
        }
        if !in_extensions {
            continue;
        }
        let Some((key, value)) = git_config_key_value(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("objectFormat") {
            let value = trim_git_config_value(value);
            if !value.is_empty() {
                return Ok(Some(value.to_string()));
            }
        }
    }
    Ok(None)
}

fn git_config_key_value(line: &str) -> Option<(&str, &str)> {
    if let Some((key, value)) = line.split_once('=') {
        return Some((key.trim(), value.trim()));
    }
    let key_end = line.find(char::is_whitespace)?;
    Some((line[..key_end].trim(), line[key_end..].trim()))
}

fn trim_git_config_value(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn resolve_head_oid(git_dir: &Path, object_format: GitObjectFormat) -> Result<String> {
    let head = read_text_file_with_limit(&git_dir.join("HEAD"), GIT_MARKER_MAX_BYTES, "read HEAD")?;
    resolve_ref_value(git_dir, "HEAD", head.trim(), object_format, 0)
}

fn resolve_ref_oid_inner(
    git_dir: &Path,
    reference: &str,
    object_format: GitObjectFormat,
    depth: usize,
) -> Result<String> {
    if depth > GIT_SYMBOLIC_REF_MAX_DEPTH {
        return Err(git_error(format!(
            "Git symbolic ref chain exceeded maximum depth while resolving {reference}"
        )));
    }
    if reference.is_empty()
        || reference.starts_with('/')
        || reference
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(git_error(format!("Invalid HEAD reference {reference:?}")));
    }
    if let Some(value) = read_loose_ref_value(git_dir, reference)? {
        return resolve_ref_value(git_dir, reference, &value, object_format, depth);
    }
    if let Some(common_dir) = common_git_dir(git_dir)? {
        if common_dir != git_dir {
            if let Some(value) = read_loose_ref_value(&common_dir, reference)? {
                return resolve_ref_value(git_dir, reference, &value, object_format, depth);
            }
        }
    }
    if let Some(oid) = read_packed_ref_oid(git_dir, reference, object_format)? {
        return Ok(oid);
    }
    if let Some(common_dir) = common_git_dir(git_dir)? {
        if common_dir != git_dir {
            if let Some(oid) = read_packed_ref_oid(&common_dir, reference, object_format)? {
                return Ok(oid);
            }
        }
    }
    Err(git_error(format!("Git ref {reference} not found")))
}

fn resolve_ref_value(
    git_dir: &Path,
    label: &str,
    value: &str,
    object_format: GitObjectFormat,
    depth: usize,
) -> Result<String> {
    if let Some(reference) = value.strip_prefix("ref:") {
        return resolve_ref_oid_inner(git_dir, reference.trim(), object_format, depth + 1);
    }
    validate_oid_hex(object_format, value)
        .map_err(|_| git_error(format!("Git ref {label} has invalid value")))?;
    Ok(value.to_string())
}

fn read_loose_ref_value(git_dir: &Path, reference: &str) -> Result<Option<String>> {
    let ref_path = git_dir.join(path_from_forward_slashes(reference));
    match read_text_file_with_limit(&ref_path, GIT_MARKER_MAX_BYTES, "read git ref") {
        Ok(content) => Ok(Some(content.trim().to_string())),
        Err(ArchivaError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn read_packed_ref_oid(
    git_dir: &Path,
    reference: &str,
    object_format: GitObjectFormat,
) -> Result<Option<String>> {
    let packed_refs = match read_text_file_with_limit(
        &git_dir.join("packed-refs"),
        GIT_OUTPUT_MAX_BYTES,
        "read packed refs",
    ) {
        Ok(content) => content,
        Err(ArchivaError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    for line in packed_refs.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(oid) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        if name == reference {
            validate_oid_hex(object_format, oid)?;
            return Ok(Some(oid.to_string()));
        }
    }
    Ok(None)
}

fn read_loose_git_object_from_dir(
    object_dir: &Path,
    oid: &str,
    object_format: GitObjectFormat,
) -> Result<GitObject> {
    validate_oid_hex(object_format, oid)?;
    let path = object_dir.join(&oid[..2]).join(&oid[2..]);
    let compressed =
        read_binary_file_with_limit(&path, GIT_OBJECT_STORAGE_MAX_BYTES, "read git object")?;
    let inflated = zlib_inflate(&compressed, GIT_OBJECT_STORAGE_MAX_BYTES)?;
    let Some(header_end) = inflated.iter().position(|byte| *byte == 0) else {
        return Err(git_error(format!("Git object {oid} is missing its header")));
    };
    let header = std::str::from_utf8(&inflated[..header_end])
        .map_err(|source| git_error(format!("Git object {oid} header is not UTF-8: {source}")))?;
    let Some((kind, size)) = header.split_once(' ') else {
        return Err(git_error(format!("Git object {oid} has invalid header")));
    };
    let size = size.parse::<usize>().map_err(|source| {
        git_error(format!(
            "Git object {oid} has invalid size in header: {source}"
        ))
    })?;
    if size > GIT_OUTPUT_MAX_BYTES {
        return Err(git_error(format!(
            "Git object {oid} size {size} exceeds {GIT_OUTPUT_MAX_BYTES} bytes"
        )));
    }
    let data = inflated[header_end + 1..].to_vec();
    if data.len() != size {
        return Err(git_error(format!(
            "Git object {oid} size mismatch: header={size} actual={}",
            data.len()
        )));
    }
    Ok(GitObject {
        kind: kind.to_string(),
        data,
    })
}

fn git_object_dirs(git_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    collect_object_dir(&git_dir.join("objects"), &mut dirs, &mut seen, 0)?;
    if let Some(common_dir) = common_git_dir(git_dir)? {
        collect_object_dir(&common_dir.join("objects"), &mut dirs, &mut seen, 0)?;
    }
    Ok(dirs)
}

fn common_git_dir(git_dir: &Path) -> Result<Option<PathBuf>> {
    let path = git_dir.join("commondir");
    let content = match read_text_file_with_limit(&path, GIT_MARKER_MAX_BYTES, "read git commondir")
    {
        Ok(content) => content,
        Err(ArchivaError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let Some(line) = content
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    else {
        return Err(git_error("Invalid git commondir marker"));
    };
    let path = PathBuf::from(line);
    if path.is_absolute() {
        Ok(Some(path))
    } else {
        Ok(Some(git_dir.join(path)))
    }
}

fn collect_object_dir(
    object_dir: &Path,
    dirs: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<()> {
    if depth > GIT_ALTERNATES_MAX_DEPTH {
        return Err(git_error("Git alternates chain exceeded maximum depth"));
    }
    let Ok(metadata) = fs::metadata(object_dir) else {
        return Ok(());
    };
    if !metadata.is_dir() {
        return Ok(());
    }
    let key = object_dir
        .canonicalize()
        .unwrap_or_else(|_| object_dir.to_path_buf());
    if !seen.insert(key) {
        return Ok(());
    }
    dirs.push(object_dir.to_path_buf());
    for alternate in alternate_object_dirs(object_dir)? {
        collect_object_dir(&alternate, dirs, seen, depth + 1)?;
    }
    Ok(())
}

fn alternate_object_dirs(object_dir: &Path) -> Result<Vec<PathBuf>> {
    let path = object_dir.join("info").join("alternates");
    let content =
        match read_text_file_with_limit(&path, GIT_OUTPUT_MAX_BYTES, "read git alternates") {
            Ok(content) => content,
            Err(ArchivaError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
    let mut alternates = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = PathBuf::from(line);
        if path.is_absolute() {
            alternates.push(path);
        } else {
            alternates.push(object_dir.join(path));
        }
    }
    Ok(alternates)
}

#[cfg(test)]
fn read_git_object(git_dir: &Path, oid: &str) -> Result<GitObject> {
    let mut context = GitObjectReadContext::default();
    let object_format = git_object_format(git_dir)?;
    read_git_object_with_context(git_dir, oid, object_format, &mut context)
}

fn read_git_object_with_context(
    git_dir: &Path,
    oid: &str,
    object_format: GitObjectFormat,
    context: &mut GitObjectReadContext,
) -> Result<GitObject> {
    read_git_object_inner(git_dir, oid, object_format, 0, context)
}

fn read_git_object_inner(
    git_dir: &Path,
    oid: &str,
    object_format: GitObjectFormat,
    depth: usize,
    context: &mut GitObjectReadContext,
) -> Result<GitObject> {
    validate_oid_hex(object_format, oid)?;
    for object_dir in git_object_dirs(git_dir)? {
        match read_git_object_from_dir(git_dir, &object_dir, oid, object_format, depth, context) {
            Ok(object) => {
                verify_git_object_hash(oid, &object, object_format)?;
                return Ok(object);
            }
            Err(ArchivaError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
            Err(ArchivaError::Git { message }) if message.ends_with(" not found") => {}
            Err(error) => return Err(error),
        }
    }
    Err(git_error(format!("Git object {oid} not found")))
}

fn read_git_object_from_dir(
    git_dir: &Path,
    object_dir: &Path,
    oid: &str,
    object_format: GitObjectFormat,
    depth: usize,
    context: &mut GitObjectReadContext,
) -> Result<GitObject> {
    match read_loose_git_object_from_dir(object_dir, oid, object_format) {
        Ok(object) => Ok(object),
        Err(ArchivaError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            read_packed_git_object_from_dir(git_dir, object_dir, oid, object_format, depth, context)
        }
        Err(error) => Err(error),
    }
}

fn read_packed_git_object_from_dir(
    git_dir: &Path,
    object_dir: &Path,
    oid: &str,
    object_format: GitObjectFormat,
    depth: usize,
    context: &mut GitObjectReadContext,
) -> Result<GitObject> {
    let oid_bytes = oid_hex_to_bytes(object_format, oid)?;
    let Some(location) = find_packed_object(object_dir, &oid_bytes, object_format, context)? else {
        return Err(git_error(format!("Git object {oid} not found")));
    };
    read_pack_object_at(
        git_dir,
        &location,
        location.offset,
        location.next_offset,
        object_format,
        depth,
        context,
    )
}

fn find_packed_object(
    object_dir: &Path,
    oid: &[u8],
    object_format: GitObjectFormat,
    context: &mut GitObjectReadContext,
) -> Result<Option<PackObjectLocation>> {
    let pack_dir = object_dir.join("pack");
    let entries = match fs::read_dir(&pack_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ArchivaError::io(
                Some(pack_dir),
                "read git pack directory",
                source,
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| {
            ArchivaError::io(Some(pack_dir.clone()), "read git pack directory", source)
        })?;
        let path = entry.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| extension != "idx")
        {
            continue;
        }
        if let Some(location) = find_object_in_pack_index(&path, oid, object_format, context)? {
            return Ok(Some(location));
        }
    }
    Ok(None)
}

fn find_object_in_pack_index(
    idx_path: &Path,
    oid: &[u8],
    object_format: GitObjectFormat,
    context: &mut GitObjectReadContext,
) -> Result<Option<PackObjectLocation>> {
    let mut index_file = fs::File::open(idx_path).map_err(|source| {
        ArchivaError::io(Some(idx_path.to_path_buf()), "open git pack index", source)
    })?;
    let index_len = index_file
        .metadata()
        .map_err(|source| {
            ArchivaError::io(
                Some(idx_path.to_path_buf()),
                "read git pack index metadata",
                source,
            )
        })?
        .len();
    let format = read_pack_index_format(&mut index_file, idx_path)?;
    validate_pack_index_checksum_once(context, idx_path, index_len, object_format)?;

    let fanout = read_pack_index_fanout(&mut index_file, idx_path, format)?;
    validate_pack_index_fanout(idx_path, &fanout)?;
    let object_count = fanout[255];
    let layout = pack_index_layout(idx_path, format, object_count, object_format)?;
    validate_pack_index_layout_once(
        context,
        &mut index_file,
        idx_path,
        index_len,
        layout,
        &fanout,
    )?;
    let prefix = usize::from(oid[0]);
    let mut low = if prefix == 0 { 0 } else { fanout[prefix - 1] };
    let mut high = fanout[prefix];
    if low == high {
        return Ok(None);
    }

    while low < high {
        let mid = low + (high - low) / 2;
        let mut candidate = vec![0_u8; object_format.raw_len()];
        read_exact_at(
            &mut index_file,
            idx_path,
            pack_index_object_name_offset(layout, mid),
            &mut candidate,
            "read git pack index object id",
        )?;
        match candidate.as_slice().cmp(oid) {
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
            std::cmp::Ordering::Equal => {
                validate_pack_index_layout_once(
                    context,
                    &mut index_file,
                    idx_path,
                    index_len,
                    layout,
                    &fanout,
                )?;
                let offset = read_pack_index_object_offset(&mut index_file, idx_path, layout, mid)?;
                let pack_path = idx_path.with_extension("pack");
                validate_pack_trailer_matches_index_once(
                    context,
                    idx_path,
                    &mut index_file,
                    index_len,
                    &pack_path,
                    object_format,
                )?;
                let pack_data_end = pack_data_end(&pack_path, object_format)?;
                let next_offset = next_pack_object_offset(
                    context,
                    &mut index_file,
                    idx_path,
                    layout,
                    offset,
                    pack_data_end,
                )?;
                return Ok(Some(PackObjectLocation {
                    idx_path: idx_path.to_path_buf(),
                    pack_path,
                    offset,
                    next_offset,
                    pack_data_end,
                    index_layout: layout,
                }));
            }
        }
    }
    Ok(None)
}

fn read_pack_index_format(index: &mut fs::File, idx_path: &Path) -> Result<PackIndexFormat> {
    let mut header = [0_u8; 8];
    read_exact_at(index, idx_path, 0, &mut header, "read git pack index")?;
    if &header[..4] != GIT_PACK_INDEX_MAGIC {
        return Ok(PackIndexFormat::V1);
    }
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if version != GIT_PACK_INDEX_VERSION {
        return Err(git_error(format!(
            "Git pack index {} has unsupported version {version}",
            idx_path.display()
        )));
    }
    Ok(PackIndexFormat::V2)
}

fn pack_index_fanout_offset(format: PackIndexFormat) -> u64 {
    match format {
        PackIndexFormat::V1 => 0,
        PackIndexFormat::V2 => GIT_PACK_INDEX_HEADER_BYTES,
    }
}

fn read_pack_index_fanout(
    index: &mut fs::File,
    idx_path: &Path,
    format: PackIndexFormat,
) -> Result<[u32; 256]> {
    let mut fanout = [0_u32; 256];
    let fanout_offset = pack_index_fanout_offset(format);
    for (fanout_index, slot) in fanout.iter_mut().enumerate() {
        *slot = read_u32_be_at(
            index,
            idx_path,
            fanout_offset + (fanout_index as u64 * 4),
            "read git pack index fanout",
        )?;
    }
    Ok(fanout)
}

fn validate_pack_index_fanout(idx_path: &Path, fanout: &[u32; 256]) -> Result<()> {
    for pair in fanout.windows(2) {
        if pair[0] > pair[1] {
            return Err(git_error(format!(
                "Git pack index {} fanout is not sorted",
                idx_path.display()
            )));
        }
    }
    Ok(())
}

fn pack_index_layout(
    idx_path: &Path,
    format: PackIndexFormat,
    object_count: u32,
    object_format: GitObjectFormat,
) -> Result<PackIndexLayout> {
    match format {
        PackIndexFormat::V1 => Ok(PackIndexLayout {
            format,
            object_format,
            object_count,
            names_offset: GIT_PACK_INDEX_FANOUT_BYTES + GIT_PACK_INDEX_OFFSET_BYTES,
            offset_table_offset: GIT_PACK_INDEX_FANOUT_BYTES,
            large_offset_table_offset: None,
        }),
        PackIndexFormat::V2 => {
            let names_offset = GIT_PACK_INDEX_HEADER_BYTES + GIT_PACK_INDEX_FANOUT_BYTES;
            let object_table_bytes = u64::from(object_count)
                .checked_mul(object_format.raw_len_u64() + GIT_PACK_INDEX_CRC_BYTES)
                .ok_or_else(|| {
                    git_error(format!(
                        "Git pack index {} sections overflow",
                        idx_path.display()
                    ))
                })?;
            let offset_table_offset =
                names_offset
                    .checked_add(object_table_bytes)
                    .ok_or_else(|| {
                        git_error(format!(
                            "Git pack index {} sections overflow",
                            idx_path.display()
                        ))
                    })?;
            let offset_table_bytes = u64::from(object_count)
                .checked_mul(GIT_PACK_INDEX_OFFSET_BYTES)
                .ok_or_else(|| {
                    git_error(format!(
                        "Git pack index {} sections overflow",
                        idx_path.display()
                    ))
                })?;
            let large_offset_table_offset = offset_table_offset
                .checked_add(offset_table_bytes)
                .ok_or_else(|| {
                    git_error(format!(
                        "Git pack index {} sections overflow",
                        idx_path.display()
                    ))
                })?;
            Ok(PackIndexLayout {
                format,
                object_format,
                object_count,
                names_offset,
                offset_table_offset,
                large_offset_table_offset: Some(large_offset_table_offset),
            })
        }
    }
}

fn pack_index_object_name_offset(layout: PackIndexLayout, object_index: u32) -> u64 {
    match layout.format {
        PackIndexFormat::V1 => {
            layout.offset_table_offset
                + u64::from(object_index) * layout.object_format.pack_index_v1_entry_bytes()
                + GIT_PACK_INDEX_OFFSET_BYTES
        }
        PackIndexFormat::V2 => {
            layout.names_offset + u64::from(object_index) * layout.object_format.raw_len_u64()
        }
    }
}

fn pack_index_object_offset_field_offset(layout: PackIndexLayout, object_index: u32) -> u64 {
    match layout.format {
        PackIndexFormat::V1 => {
            layout.offset_table_offset
                + u64::from(object_index) * layout.object_format.pack_index_v1_entry_bytes()
        }
        PackIndexFormat::V2 => {
            layout.offset_table_offset + u64::from(object_index) * GIT_PACK_INDEX_OFFSET_BYTES
        }
    }
}

fn validate_pack_index_layout_once(
    context: &mut GitObjectReadContext,
    index: &mut fs::File,
    idx_path: &Path,
    index_len: u64,
    layout: PackIndexLayout,
    fanout: &[u32; 256],
) -> Result<()> {
    if context.validated_index_layouts.contains(idx_path) {
        return Ok(());
    }
    validate_pack_index_layout(index, idx_path, index_len, layout, fanout)?;
    context
        .validated_index_layouts
        .insert(idx_path.to_path_buf());
    Ok(())
}

fn validate_pack_index_layout(
    index: &mut fs::File,
    idx_path: &Path,
    index_len: u64,
    layout: PackIndexLayout,
    fanout: &[u32; 256],
) -> Result<()> {
    let Some(trailer_offset) =
        index_len.checked_sub(layout.object_format.pack_index_trailer_bytes())
    else {
        return Err(git_error(format!(
            "Git pack index {} is truncated",
            idx_path.display()
        )));
    };
    match layout.format {
        PackIndexFormat::V1 => {
            let entries_bytes = u64::from(layout.object_count)
                .checked_mul(layout.object_format.pack_index_v1_entry_bytes())
                .ok_or_else(|| {
                    git_error(format!(
                        "Git pack index {} sections overflow",
                        idx_path.display()
                    ))
                })?;
            let expected_trailer_offset = layout
                .offset_table_offset
                .checked_add(entries_bytes)
                .ok_or_else(|| {
                    git_error(format!(
                        "Git pack index {} sections overflow",
                        idx_path.display()
                    ))
                })?;
            if expected_trailer_offset != trailer_offset {
                return Err(git_error(format!(
                    "Git pack index {} v1 sections do not match file length",
                    idx_path.display()
                )));
            }
            validate_pack_index_object_names(index, idx_path, layout, fanout)?;
            return Ok(());
        }
        PackIndexFormat::V2 => {}
    }

    let large_offset_table_offset = layout.large_offset_table_offset.ok_or_else(|| {
        git_error(format!(
            "Git pack index {} sections are inconsistent",
            idx_path.display()
        ))
    })?;
    let offset_table_bytes = u64::from(layout.object_count) * GIT_PACK_INDEX_OFFSET_BYTES;
    let expected_large_offset_table_offset = layout
        .offset_table_offset
        .checked_add(offset_table_bytes)
        .ok_or_else(|| {
            git_error(format!(
                "Git pack index {} sections overflow",
                idx_path.display()
            ))
        })?;
    if expected_large_offset_table_offset != large_offset_table_offset {
        return Err(git_error(format!(
            "Git pack index {} sections are inconsistent",
            idx_path.display()
        )));
    }
    if large_offset_table_offset > trailer_offset {
        return Err(git_error(format!(
            "Git pack index {} sections exceed file length",
            idx_path.display()
        )));
    }
    let large_offset_table_bytes = trailer_offset - large_offset_table_offset;
    if !large_offset_table_bytes.is_multiple_of(GIT_PACK_INDEX_LARGE_OFFSET_BYTES) {
        return Err(git_error(format!(
            "Git pack index {} large offset table is misaligned",
            idx_path.display()
        )));
    }
    let large_offset_entries = large_offset_table_bytes / GIT_PACK_INDEX_LARGE_OFFSET_BYTES;
    let mut large_offset_references = 0_u64;
    let mut seen_large_offsets = HashSet::new();
    for object_index in 0..layout.object_count {
        let value = read_u32_be_at(
            index,
            idx_path,
            pack_index_object_offset_field_offset(layout, object_index),
            "read git pack object offset",
        )?;
        if value & 0x8000_0000 == 0 {
            continue;
        }
        let large_index = u64::from(value & 0x7fff_ffff);
        if large_index >= large_offset_entries {
            return Err(git_error(format!(
                "Git pack index {} large offset table reference {large_index} exceeds {large_offset_entries} entries",
                idx_path.display()
            )));
        }
        if !seen_large_offsets.insert(large_index) {
            return Err(git_error(format!(
                "Git pack index {} duplicate large offset table reference {large_index}",
                idx_path.display()
            )));
        }
        large_offset_references += 1;
    }
    if large_offset_references != large_offset_entries {
        return Err(git_error(format!(
            "Git pack index {} large offset table has {large_offset_entries} entries but {large_offset_references} references",
            idx_path.display()
        )));
    }
    validate_pack_index_object_names(index, idx_path, layout, fanout)?;
    Ok(())
}

fn validate_pack_index_object_names(
    index: &mut fs::File,
    idx_path: &Path,
    layout: PackIndexLayout,
    fanout: &[u32; 256],
) -> Result<()> {
    let mut bucket_counts = [0_u32; 256];
    let mut previous: Option<Vec<u8>> = None;
    for object_index in 0..layout.object_count {
        let mut candidate = vec![0_u8; layout.object_format.raw_len()];
        read_exact_at(
            index,
            idx_path,
            pack_index_object_name_offset(layout, object_index),
            &mut candidate,
            "read git pack index object id",
        )?;
        if previous
            .as_ref()
            .is_some_and(|previous| previous.as_slice() >= candidate.as_slice())
        {
            return Err(git_error(format!(
                "Git pack index {} object ids are not sorted",
                idx_path.display()
            )));
        }
        bucket_counts[usize::from(candidate[0])] += 1;
        previous = Some(candidate);
    }

    let mut cumulative = 0_u32;
    for (bucket, count) in bucket_counts.iter().enumerate() {
        cumulative = cumulative.checked_add(*count).ok_or_else(|| {
            git_error(format!(
                "Git pack index {} fanout count overflow",
                idx_path.display()
            ))
        })?;
        if cumulative != fanout[bucket] {
            return Err(git_error(format!(
                "Git pack index {} fanout does not match object id table",
                idx_path.display()
            )));
        }
    }
    Ok(())
}

fn validate_pack_index_checksum_once(
    context: &mut GitObjectReadContext,
    idx_path: &Path,
    index_len: u64,
    object_format: GitObjectFormat,
) -> Result<()> {
    if context.validated_index_checksums.contains(idx_path) {
        return Ok(());
    }
    validate_pack_index_checksum(idx_path, index_len, object_format)?;
    context
        .validated_index_checksums
        .insert(idx_path.to_path_buf());
    Ok(())
}

fn validate_pack_index_checksum(
    idx_path: &Path,
    index_len: u64,
    object_format: GitObjectFormat,
) -> Result<()> {
    let Some(checksum_offset) = index_len.checked_sub(object_format.raw_len_u64()) else {
        return Err(git_error(format!(
            "Git pack index {} is truncated",
            idx_path.display()
        )));
    };
    let actual = git_hash_file_prefix(
        object_format,
        idx_path,
        checksum_offset,
        "hash git pack index",
    )?;
    let mut expected = vec![0_u8; object_format.raw_len()];
    let mut index = fs::File::open(idx_path).map_err(|source| {
        ArchivaError::io(Some(idx_path.to_path_buf()), "open git pack index", source)
    })?;
    read_exact_at(
        &mut index,
        idx_path,
        checksum_offset,
        &mut expected,
        "read git pack index checksum",
    )?;
    if actual != expected {
        return Err(git_error(format!(
            "Git pack index {} checksum mismatch",
            idx_path.display()
        )));
    }
    Ok(())
}

fn validate_pack_trailer_matches_index_once(
    context: &mut GitObjectReadContext,
    idx_path: &Path,
    index_file: &mut fs::File,
    index_len: u64,
    pack_path: &Path,
    object_format: GitObjectFormat,
) -> Result<()> {
    if context.validated_index_pack_pairs.contains(idx_path) {
        return Ok(());
    }
    validate_pack_trailer_matches_index(idx_path, index_file, index_len, pack_path, object_format)?;
    context
        .validated_index_pack_pairs
        .insert(idx_path.to_path_buf());
    Ok(())
}

fn validate_pack_trailer_matches_index(
    idx_path: &Path,
    index_file: &mut fs::File,
    index_len: u64,
    pack_path: &Path,
    object_format: GitObjectFormat,
) -> Result<()> {
    let pack_checksum_offset = index_len
        .checked_sub(object_format.pack_index_trailer_bytes())
        .ok_or_else(|| {
            git_error(format!(
                "Git pack index {} is truncated",
                idx_path.display()
            ))
        })?;
    let mut index_pack_checksum = vec![0_u8; object_format.raw_len()];
    read_exact_at(
        index_file,
        idx_path,
        pack_checksum_offset,
        &mut index_pack_checksum,
        "read git pack checksum from index",
    )?;
    let pack_len = fs::metadata(pack_path)
        .map_err(|source| {
            ArchivaError::io(
                Some(pack_path.to_path_buf()),
                "read git pack metadata",
                source,
            )
        })?
        .len();
    let pack_trailer_offset = pack_len
        .checked_sub(object_format.raw_len_u64())
        .ok_or_else(|| git_error(format!("Git pack {} is truncated", pack_path.display())))?;
    let mut pack_trailer = vec![0_u8; object_format.raw_len()];
    let mut pack = fs::File::open(pack_path).map_err(|source| {
        ArchivaError::io(Some(pack_path.to_path_buf()), "open git pack file", source)
    })?;
    read_exact_at(
        &mut pack,
        pack_path,
        pack_trailer_offset,
        &mut pack_trailer,
        "read git pack trailer checksum",
    )?;
    let actual_pack_checksum = git_hash_file_prefix(
        object_format,
        pack_path,
        pack_trailer_offset,
        "hash git pack",
    )?;
    if actual_pack_checksum != pack_trailer {
        return Err(git_error(format!(
            "Git pack {} trailer checksum mismatch",
            pack_path.display()
        )));
    }
    if pack_trailer != index_pack_checksum {
        return Err(git_error(format!(
            "Git pack {} checksum does not match index {}",
            pack_path.display(),
            idx_path.display()
        )));
    }
    Ok(())
}

fn read_pack_index_object_offset(
    index: &mut fs::File,
    idx_path: &Path,
    layout: PackIndexLayout,
    object_index: u32,
) -> Result<u64> {
    let value = read_u32_be_at(
        index,
        idx_path,
        pack_index_object_offset_field_offset(layout, object_index),
        "read git pack object offset",
    )?;
    if layout.format == PackIndexFormat::V1 {
        return Ok(u64::from(value));
    }
    if value & 0x8000_0000 == 0 {
        return Ok(u64::from(value));
    }
    let large_index = value & 0x7fff_ffff;
    let large_offset_table_offset = layout.large_offset_table_offset.ok_or_else(|| {
        git_error(format!(
            "Git pack index {} sections are inconsistent",
            idx_path.display()
        ))
    })?;
    read_u64_be_at(
        index,
        idx_path,
        large_offset_table_offset + u64::from(large_index) * GIT_PACK_INDEX_LARGE_OFFSET_BYTES,
        "read git pack large object offset",
    )
}

fn next_pack_object_offset(
    context: &mut GitObjectReadContext,
    index: &mut fs::File,
    idx_path: &Path,
    layout: PackIndexLayout,
    offset: u64,
    pack_data_end: u64,
) -> Result<u64> {
    let offsets = sorted_pack_index_offsets_once(context, index, idx_path, layout)?;
    let mut low = 0_usize;
    let mut high = offsets.len();
    while low < high {
        let mid = low + (high - low) / 2;
        if offsets[mid] <= offset {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    Ok(offsets.get(low).copied().unwrap_or(pack_data_end))
}

fn sorted_pack_index_offsets_once<'a>(
    context: &'a mut GitObjectReadContext,
    index: &mut fs::File,
    idx_path: &Path,
    layout: PackIndexLayout,
) -> Result<&'a [u64]> {
    if !context.pack_index_offsets.contains_key(idx_path) {
        let mut offsets = Vec::with_capacity(layout.object_count as usize);
        for object_index in 0..layout.object_count {
            offsets.push(read_pack_index_object_offset(
                index,
                idx_path,
                layout,
                object_index,
            )?);
        }
        offsets.sort_unstable();
        context
            .pack_index_offsets
            .insert(idx_path.to_path_buf(), offsets);
    }
    Ok(context
        .pack_index_offsets
        .get(idx_path)
        .map(Vec::as_slice)
        .expect("pack index offsets inserted before lookup"))
}

fn pack_data_end(pack_path: &Path, object_format: GitObjectFormat) -> Result<u64> {
    let metadata = fs::metadata(pack_path).map_err(|source| {
        ArchivaError::io(
            Some(pack_path.to_path_buf()),
            "read git pack metadata",
            source,
        )
    })?;
    metadata
        .len()
        .checked_sub(object_format.raw_len_u64())
        .ok_or_else(|| git_error(format!("Git pack {} is truncated", pack_path.display())))
}

fn read_pack_object_at(
    git_dir: &Path,
    location: &PackObjectLocation,
    offset: u64,
    next_offset: u64,
    object_format: GitObjectFormat,
    depth: usize,
    context: &mut GitObjectReadContext,
) -> Result<GitObject> {
    if depth > GIT_PACK_DELTA_MAX_DEPTH {
        return Err(git_error("Git pack delta chain exceeded maximum depth"));
    }
    if offset < 12 || offset >= location.pack_data_end || next_offset > location.pack_data_end {
        return Err(git_error(format!(
            "Git pack object offset {offset} is outside {}",
            location.pack_path.display()
        )));
    }
    let mut pack = fs::File::open(&location.pack_path).map_err(|source| {
        ArchivaError::io(
            Some(location.pack_path.clone()),
            "open git pack file",
            source,
        )
    })?;
    validate_pack_header(
        &mut pack,
        &location.pack_path,
        location.index_layout.object_count,
    )?;
    pack.seek(SeekFrom::Start(offset)).map_err(|source| {
        ArchivaError::io(
            Some(location.pack_path.clone()),
            "seek git pack object",
            source,
        )
    })?;
    let (object_type, expected_size) = read_pack_object_header(&mut pack, &location.pack_path)?;
    let mut compressed_start = pack.stream_position().map_err(|source| {
        ArchivaError::io(
            Some(location.pack_path.clone()),
            "read git pack object position",
            source,
        )
    })?;

    match object_type {
        1..=4 => {
            let data = read_pack_inflated_data(
                &location.pack_path,
                compressed_start,
                next_offset,
                expected_size,
            )?;
            Ok(GitObject {
                kind: pack_object_kind(object_type)?.to_string(),
                data,
            })
        }
        6 => {
            let base_offset_delta = read_pack_ofs_delta_base(&mut pack, &location.pack_path)?;
            compressed_start = pack.stream_position().map_err(|source| {
                ArchivaError::io(
                    Some(location.pack_path.clone()),
                    "read git pack object position",
                    source,
                )
            })?;
            let base_offset = offset.checked_sub(base_offset_delta).ok_or_else(|| {
                git_error(format!(
                    "Git pack OFS_DELTA base offset before start of {}",
                    location.pack_path.display()
                ))
            })?;
            let mut index = fs::File::open(&location.idx_path).map_err(|source| {
                ArchivaError::io(
                    Some(location.idx_path.clone()),
                    "open git pack index",
                    source,
                )
            })?;
            let base_next_offset = next_pack_object_offset(
                context,
                &mut index,
                &location.idx_path,
                location.index_layout,
                base_offset,
                location.pack_data_end,
            )?;
            let base = read_pack_object_at(
                git_dir,
                location,
                base_offset,
                base_next_offset,
                object_format,
                depth + 1,
                context,
            )?;
            let delta = read_pack_inflated_data(
                &location.pack_path,
                compressed_start,
                next_offset,
                expected_size,
            )?;
            Ok(GitObject {
                kind: base.kind,
                data: apply_git_delta(&base.data, &delta)?,
            })
        }
        7 => {
            let mut base_oid = vec![0_u8; object_format.raw_len()];
            pack.read_exact(&mut base_oid).map_err(|source| {
                ArchivaError::io(
                    Some(location.pack_path.clone()),
                    "read git REF_DELTA base id",
                    source,
                )
            })?;
            compressed_start = pack.stream_position().map_err(|source| {
                ArchivaError::io(
                    Some(location.pack_path.clone()),
                    "read git pack object position",
                    source,
                )
            })?;
            let base_oid = bytes_to_hex(&base_oid);
            let base =
                read_git_object_inner(git_dir, &base_oid, object_format, depth + 1, context)?;
            let delta = read_pack_inflated_data(
                &location.pack_path,
                compressed_start,
                next_offset,
                expected_size,
            )?;
            Ok(GitObject {
                kind: base.kind,
                data: apply_git_delta(&base.data, &delta)?,
            })
        }
        _ => Err(git_error(format!(
            "Git pack object at offset {offset} has invalid type {object_type}"
        ))),
    }
}

fn validate_pack_header(
    pack: &mut fs::File,
    pack_path: &Path,
    expected_object_count: u32,
) -> Result<()> {
    let mut header = [0_u8; 12];
    read_exact_at(pack, pack_path, 0, &mut header, "read git pack header")?;
    if &header[..4] != GIT_PACK_SIGNATURE {
        return Err(git_error(format!(
            "Git pack {} has invalid signature",
            pack_path.display()
        )));
    }
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if !(2..=3).contains(&version) {
        return Err(git_error(format!(
            "Git pack {} has unsupported version {version}",
            pack_path.display()
        )));
    }
    let object_count = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    if object_count != expected_object_count {
        return Err(git_error(format!(
            "Git pack {} object count {object_count} does not match index count {expected_object_count}",
            pack_path.display()
        )));
    }
    Ok(())
}

fn read_pack_object_header(pack: &mut fs::File, pack_path: &Path) -> Result<(u8, usize)> {
    let mut byte = read_pack_byte(pack, pack_path, "read git pack object header")?;
    let object_type = (byte >> 4) & 0x07;
    let mut size = usize::from(byte & 0x0f);
    let mut shift = 4_usize;
    while byte & 0x80 != 0 {
        byte = read_pack_byte(pack, pack_path, "read git pack object header")?;
        let chunk = usize::from(byte & 0x7f);
        size = size
            .checked_add(
                chunk
                    .checked_shl(shift as u32)
                    .ok_or_else(|| git_error("Git pack object size overflow"))?,
            )
            .ok_or_else(|| git_error("Git pack object size overflow"))?;
        shift = shift
            .checked_add(7)
            .ok_or_else(|| git_error("Git pack object size overflow"))?;
        if shift >= usize::BITS as usize {
            return Err(git_error("Git pack object size overflow"));
        }
    }
    let size_limit = match object_type {
        6 | 7 => GIT_OBJECT_STORAGE_MAX_BYTES,
        _ => GIT_OUTPUT_MAX_BYTES,
    };
    if size > size_limit {
        return Err(git_error(format!(
            "Git pack object inflated size {size} exceeds {size_limit} bytes"
        )));
    }
    Ok((object_type, size))
}

fn read_pack_ofs_delta_base(pack: &mut fs::File, pack_path: &Path) -> Result<u64> {
    let mut byte = read_pack_byte(pack, pack_path, "read git OFS_DELTA base offset")?;
    let mut offset = u64::from(byte & 0x7f);
    while byte & 0x80 != 0 {
        byte = read_pack_byte(pack, pack_path, "read git OFS_DELTA base offset")?;
        offset = offset
            .checked_add(1)
            .and_then(|value| value.checked_shl(7))
            .and_then(|value| value.checked_add(u64::from(byte & 0x7f)))
            .ok_or_else(|| git_error("Git OFS_DELTA base offset overflow"))?;
    }
    Ok(offset)
}

fn read_pack_inflated_data(
    pack_path: &Path,
    start: u64,
    end: u64,
    expected_size: usize,
) -> Result<Vec<u8>> {
    if end < start {
        return Err(git_error(format!(
            "Git pack object has invalid compressed range in {}",
            pack_path.display()
        )));
    }
    let compressed = read_binary_range_with_limit(
        pack_path,
        start,
        end - start,
        GIT_OBJECT_STORAGE_MAX_BYTES,
        "read git pack compressed object",
    )?;
    let inflated = zlib_inflate(&compressed, expected_size)?;
    if inflated.len() != expected_size {
        return Err(git_error(format!(
            "Git pack object size mismatch: header={expected_size} actual={}",
            inflated.len()
        )));
    }
    Ok(inflated)
}

fn apply_git_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let mut index = 0_usize;
    let source_size = read_git_delta_varint(delta, &mut index)?;
    if source_size != base.len() {
        return Err(git_error(format!(
            "Git delta source size mismatch: header={source_size} actual={}",
            base.len()
        )));
    }
    let target_size = read_git_delta_varint(delta, &mut index)?;
    if target_size > GIT_OUTPUT_MAX_BYTES {
        return Err(git_error(format!(
            "Git delta target size {target_size} exceeds {GIT_OUTPUT_MAX_BYTES} bytes"
        )));
    }
    let mut output = Vec::with_capacity(target_size);
    while index < delta.len() {
        let opcode = delta[index];
        index += 1;
        if opcode & 0x80 != 0 {
            let mut copy_offset = 0_usize;
            let mut copy_size = 0_usize;
            for byte_index in 0..4 {
                if opcode & (1 << byte_index) != 0 {
                    copy_offset |= read_delta_byte(delta, &mut index)? << (byte_index * 8);
                }
            }
            for byte_index in 0..3 {
                if opcode & (1 << (4 + byte_index)) != 0 {
                    copy_size |= read_delta_byte(delta, &mut index)? << (byte_index * 8);
                }
            }
            if copy_size == 0 {
                copy_size = 0x10000;
            }
            let copy_end = copy_offset
                .checked_add(copy_size)
                .ok_or_else(|| git_error("Git delta copy range overflow"))?;
            if copy_end > base.len() {
                return Err(git_error("Git delta copy range exceeds base object"));
            }
            if output.len().saturating_add(copy_size) > target_size {
                return Err(git_error("Git delta output exceeds target size"));
            }
            output.extend_from_slice(&base[copy_offset..copy_end]);
        } else if opcode != 0 {
            let insert_size = usize::from(opcode);
            let insert_end = index
                .checked_add(insert_size)
                .ok_or_else(|| git_error("Git delta insert range overflow"))?;
            if insert_end > delta.len() {
                return Err(git_error("Git delta insert range exceeds delta data"));
            }
            if output.len().saturating_add(insert_size) > target_size {
                return Err(git_error("Git delta output exceeds target size"));
            }
            output.extend_from_slice(&delta[index..insert_end]);
            index = insert_end;
        } else {
            return Err(git_error("Git delta contains reserved opcode 0"));
        }
    }
    if output.len() != target_size {
        return Err(git_error(format!(
            "Git delta target size mismatch: header={target_size} actual={}",
            output.len()
        )));
    }
    Ok(output)
}

fn read_git_delta_varint(delta: &[u8], index: &mut usize) -> Result<usize> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    loop {
        let byte = read_delta_byte(delta, index)?;
        value = value
            .checked_add(
                (byte & 0x7f)
                    .checked_shl(shift)
                    .ok_or_else(|| git_error("Git delta size overflow"))?,
            )
            .ok_or_else(|| git_error("Git delta size overflow"))?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift = shift
            .checked_add(7)
            .ok_or_else(|| git_error("Git delta size overflow"))?;
        if shift >= usize::BITS {
            return Err(git_error("Git delta size overflow"));
        }
    }
}

fn read_delta_byte(delta: &[u8], index: &mut usize) -> Result<usize> {
    let Some(byte) = delta.get(*index).copied() else {
        return Err(git_error("Git delta ended unexpectedly"));
    };
    *index += 1;
    Ok(usize::from(byte))
}

fn pack_object_kind(object_type: u8) -> Result<&'static str> {
    match object_type {
        1 => Ok("commit"),
        2 => Ok("tree"),
        3 => Ok("blob"),
        4 => Ok("tag"),
        _ => Err(git_error(format!(
            "Git pack object has invalid base type {object_type}"
        ))),
    }
}

fn read_pack_byte(pack: &mut fs::File, pack_path: &Path, action: &'static str) -> Result<u8> {
    let mut byte = [0_u8; 1];
    pack.read_exact(&mut byte)
        .map_err(|source| ArchivaError::io(Some(pack_path.to_path_buf()), action, source))?;
    Ok(byte[0])
}

fn read_exact_at(
    file: &mut fs::File,
    path: &Path,
    offset: u64,
    buffer: &mut [u8],
    action: &'static str,
) -> Result<()> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    file.read_exact(buffer)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))
}

fn read_u32_be_at(
    file: &mut fs::File,
    path: &Path,
    offset: u64,
    action: &'static str,
) -> Result<u32> {
    let mut bytes = [0_u8; 4];
    read_exact_at(file, path, offset, &mut bytes, action)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64_be_at(
    file: &mut fs::File,
    path: &Path,
    offset: u64,
    action: &'static str,
) -> Result<u64> {
    let mut bytes = [0_u8; 8];
    read_exact_at(file, path, offset, &mut bytes, action)?;
    Ok(u64::from_be_bytes(bytes))
}

fn read_binary_range_with_limit(
    path: &Path,
    offset: u64,
    length: u64,
    limit: usize,
    action: &'static str,
) -> Result<Vec<u8>> {
    if length > limit as u64 {
        return Err(ArchivaError::FileTooLarge {
            path: path.to_path_buf(),
            limit,
        });
    }
    let mut file = fs::File::open(path)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    let mut bytes = Vec::with_capacity(length as usize);
    Read::by_ref(&mut file)
        .take(length)
        .read_to_end(&mut bytes)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    if bytes.len() != length as usize {
        return Err(git_error(format!(
            "{} ended before {} requested bytes were read",
            path.display(),
            length
        )));
    }
    Ok(bytes)
}

fn commit_tree_oid(commit: &[u8], object_format: GitObjectFormat) -> Result<String> {
    let Some(line) = commit
        .split(|byte| *byte == b'\n')
        .find(|line| line.starts_with(b"tree "))
    else {
        return Err(git_error("HEAD commit has no tree"));
    };
    let oid = std::str::from_utf8(&line[b"tree ".len()..])
        .map_err(|source| git_error(format!("HEAD commit tree id is not UTF-8: {source}")))?;
    validate_oid_hex(object_format, oid)?;
    Ok(oid.to_string())
}

fn tree_blob_oid(
    git_dir: &Path,
    tree_oid: &str,
    git_relative_path: &str,
    object_format: GitObjectFormat,
    context: &mut GitObjectReadContext,
) -> Result<String> {
    let mut current_tree = tree_oid.to_string();
    let mut components = git_relative_path.split('/').peekable();
    while let Some(component) = components.next() {
        if component.is_empty() || component == "." || component == ".." {
            return Err(git_error(format!(
                "Invalid git path component in {git_relative_path:?}"
            )));
        }
        let tree = read_git_object_with_context(git_dir, &current_tree, object_format, context)?;
        if tree.kind != "tree" {
            return Err(git_error(format!("{current_tree} is not a tree object")));
        }
        let Some(entry) = tree_entry(&tree.data, component.as_bytes(), object_format)? else {
            return Err(git_error(format!(
                "HEAD:{git_relative_path} does not exist"
            )));
        };
        if components.peek().is_some() {
            if entry.mode != b"40000" {
                return Err(git_error(format!(
                    "HEAD:{git_relative_path} parent component is not a tree"
                )));
            }
            current_tree = bytes_to_hex(entry.oid);
        } else {
            return Ok(bytes_to_hex(entry.oid));
        }
    }
    Err(git_error("Empty git path"))
}

struct TreeEntry<'a> {
    mode: &'a [u8],
    oid: &'a [u8],
}

fn tree_entry<'a>(
    tree: &'a [u8],
    name: &[u8],
    object_format: GitObjectFormat,
) -> Result<Option<TreeEntry<'a>>> {
    let mut index = 0;
    while index < tree.len() {
        let mode_start = index;
        while index < tree.len() && tree[index] != b' ' {
            index += 1;
        }
        if index >= tree.len() {
            return Err(git_error("Malformed git tree entry mode"));
        }
        let mode = &tree[mode_start..index];
        index += 1;
        let name_start = index;
        while index < tree.len() && tree[index] != 0 {
            index += 1;
        }
        if index >= tree.len() {
            return Err(git_error("Malformed git tree entry name"));
        }
        let entry_name = &tree[name_start..index];
        index += 1;
        if index + object_format.raw_len() > tree.len() {
            return Err(git_error("Malformed git tree entry object id"));
        }
        let oid = &tree[index..index + object_format.raw_len()];
        index += object_format.raw_len();
        if entry_name == name {
            return Ok(Some(TreeEntry { mode, oid }));
        }
    }
    Ok(None)
}

fn read_binary_file_with_limit(path: &Path, limit: usize, action: &'static str) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    if bytes.len() > limit {
        return Err(ArchivaError::FileTooLarge {
            path: path.to_path_buf(),
            limit,
        });
    }
    Ok(bytes)
}

fn zlib_inflate(input: &[u8], max_output: usize) -> Result<Vec<u8>> {
    if input.len() < 6 {
        return Err(git_error("Git object zlib stream is truncated"));
    }
    let cmf = input[0];
    let flg = input[1];
    let header = (u16::from(cmf) << 8) | u16::from(flg);
    if cmf & 0x0f != 8 || cmf >> 4 > 7 || header % 31 != 0 {
        return Err(git_error("Git object has invalid zlib header"));
    }
    if flg & 0x20 != 0 {
        return Err(git_error("Git object uses unsupported zlib dictionary"));
    }
    let trailer_offset = input.len() - 4;
    let expected_adler = u32::from_be_bytes([
        input[trailer_offset],
        input[trailer_offset + 1],
        input[trailer_offset + 2],
        input[trailer_offset + 3],
    ]);
    let mut reader = BitReader::new(&input[2..trailer_offset]);
    let mut output = Vec::new();
    loop {
        let final_block = reader.read_bits(1)? == 1;
        let block_type = reader.read_bits(2)?;
        match block_type {
            0 => inflate_stored_block(&mut reader, &mut output, max_output)?,
            1 => inflate_huffman_block(
                &mut reader,
                &fixed_literal_huffman()?,
                &fixed_distance_huffman()?,
                &mut output,
                max_output,
            )?,
            2 => {
                let (literal, distance) = dynamic_huffman(&mut reader)?;
                inflate_huffman_block(&mut reader, &literal, &distance, &mut output, max_output)?;
            }
            _ => return Err(git_error("Git object uses reserved deflate block type")),
        }
        if final_block {
            break;
        }
    }
    reader.align_byte();
    if reader.byte_index != reader.bytes.len() {
        return Err(git_error(
            "Git object zlib stream has trailing deflate bytes",
        ));
    }
    let actual_adler = adler32(&output);
    if actual_adler != expected_adler {
        return Err(git_error("Git object zlib Adler-32 checksum mismatch"));
    }
    Ok(output)
}

fn inflate_stored_block(
    reader: &mut BitReader<'_>,
    output: &mut Vec<u8>,
    max_output: usize,
) -> Result<()> {
    reader.align_byte();
    let len = reader.read_u16_le()?;
    let nlen = reader.read_u16_le()?;
    if len != !nlen {
        return Err(git_error("Stored deflate block length check failed"));
    }
    for _ in 0..len {
        push_output(output, reader.read_aligned_byte()?, max_output)?;
    }
    Ok(())
}

fn inflate_huffman_block(
    reader: &mut BitReader<'_>,
    literal: &Huffman,
    distance: &Huffman,
    output: &mut Vec<u8>,
    max_output: usize,
) -> Result<()> {
    loop {
        let symbol = literal.decode(reader)?;
        match symbol {
            0..=255 => push_output(output, symbol as u8, max_output)?,
            256 => return Ok(()),
            257..=285 => {
                let index = usize::from(symbol - 257);
                let length = LENGTH_BASE[index] + reader.read_bits_usize(LENGTH_EXTRA[index])?;
                let distance_symbol = distance.decode(reader)?;
                if distance_symbol > 29 {
                    return Err(git_error("Invalid deflate distance symbol"));
                }
                let distance_index = usize::from(distance_symbol);
                let distance = DISTANCE_BASE[distance_index]
                    + reader.read_bits_usize(DISTANCE_EXTRA[distance_index])?;
                copy_from_output(output, distance, length, max_output)?;
            }
            _ => return Err(git_error("Invalid deflate literal symbol")),
        }
    }
}

fn dynamic_huffman(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman)> {
    let literal_count = reader.read_bits_usize(5)? + 257;
    let distance_count = reader.read_bits_usize(5)? + 1;
    let code_length_count = reader.read_bits_usize(4)? + 4;
    let mut code_lengths = vec![0_u8; 19];
    for index in 0..code_length_count {
        code_lengths[usize::from(CODE_LENGTH_ORDER[index])] = reader.read_bits(3)? as u8;
    }
    let code_huffman = Huffman::from_lengths(&code_lengths)?;
    let total = literal_count + distance_count;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        let symbol = code_huffman.decode(reader)?;
        match symbol {
            0..=15 => lengths.push(symbol as u8),
            16 => {
                let Some(previous) = lengths.last().copied() else {
                    return Err(git_error("Deflate repeat length has no previous value"));
                };
                let repeat = reader.read_bits_usize(2)? + 3;
                for _ in 0..repeat {
                    lengths.push(previous);
                }
            }
            17 => {
                let repeat = reader.read_bits_usize(3)? + 3;
                lengths.extend(std::iter::repeat_n(0, repeat));
            }
            18 => {
                let repeat = reader.read_bits_usize(7)? + 11;
                lengths.extend(std::iter::repeat_n(0, repeat));
            }
            _ => return Err(git_error("Invalid deflate code length symbol")),
        }
        if lengths.len() > total {
            return Err(git_error("Deflate code lengths exceed expected count"));
        }
    }
    let literal = Huffman::from_lengths(&lengths[..literal_count])?;
    if !literal.has_symbol(256) {
        return Err(git_error("Deflate literal table is missing end-of-block"));
    }
    let distance = dynamic_distance_huffman(&literal, &lengths[literal_count..])?;
    Ok((literal, distance))
}

fn dynamic_distance_huffman(literal: &Huffman, lengths: &[u8]) -> Result<Huffman> {
    if lengths.iter().all(|len| *len == 0) {
        if literal.has_symbol_range(257, 285) {
            return Err(git_error("Deflate length symbols require a distance table"));
        }
        return Ok(Huffman::empty());
    }
    Huffman::from_lengths(lengths)
}

impl Huffman {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            max_len: 0,
        }
    }

    fn from_lengths(lengths: &[u8]) -> Result<Self> {
        let max_len = lengths.iter().copied().max().unwrap_or(0);
        if max_len == 0 {
            return Err(git_error("Deflate Huffman table is empty"));
        }
        if max_len > 15 {
            return Err(git_error("Deflate Huffman code length exceeds 15 bits"));
        }
        let mut counts = vec![0_u16; usize::from(max_len) + 1];
        for len in lengths.iter().copied().filter(|len| *len > 0) {
            counts[usize::from(len)] += 1;
        }
        let mut remaining = 1_i32;
        for count in counts.iter().take(usize::from(max_len) + 1).skip(1) {
            remaining = (remaining << 1) - i32::from(*count);
            if remaining < 0 {
                return Err(git_error("Deflate Huffman table is oversubscribed"));
            }
        }
        let mut code = 0_u16;
        let mut next_codes = vec![0_u16; usize::from(max_len) + 1];
        for bits in 1..=usize::from(max_len) {
            code = (code + counts[bits - 1]) << 1;
            next_codes[bits] = code;
        }
        let mut entries = Vec::new();
        for (symbol, len) in lengths.iter().copied().enumerate() {
            if len == 0 {
                continue;
            }
            let code = next_codes[usize::from(len)];
            next_codes[usize::from(len)] += 1;
            entries.push(HuffmanEntry {
                code: reverse_bits(code, len),
                len,
                symbol: symbol as u16,
            });
        }
        Ok(Self { entries, max_len })
    }

    fn has_symbol(&self, symbol: u16) -> bool {
        self.entries.iter().any(|entry| entry.symbol == symbol)
    }

    fn has_symbol_range(&self, start: u16, end: u16) -> bool {
        self.entries
            .iter()
            .any(|entry| (start..=end).contains(&entry.symbol))
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16> {
        if self.max_len == 0 {
            return Err(git_error("Deflate Huffman table is empty"));
        }
        let mut code = 0_u16;
        for len in 1..=self.max_len {
            let bit = reader.read_bits(1)?;
            code |= bit << (len - 1);
            if let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.len == len && entry.code == code)
            {
                return Ok(entry.symbol);
            }
        }
        Err(git_error("Invalid deflate Huffman code"))
    }
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_index: 0,
            bit_index: 0,
        }
    }

    fn read_bits(&mut self, count: u8) -> Result<u16> {
        let mut value = 0_u16;
        for bit in 0..count {
            if self.byte_index >= self.bytes.len() {
                return Err(git_error("Deflate stream ended unexpectedly"));
            }
            let next = (self.bytes[self.byte_index] >> self.bit_index) & 1;
            value |= u16::from(next) << bit;
            self.bit_index += 1;
            if self.bit_index == 8 {
                self.bit_index = 0;
                self.byte_index += 1;
            }
        }
        Ok(value)
    }

    fn read_bits_usize(&mut self, count: u8) -> Result<usize> {
        Ok(usize::from(self.read_bits(count)?))
    }

    fn align_byte(&mut self) {
        if self.bit_index != 0 {
            self.bit_index = 0;
            self.byte_index += 1;
        }
    }

    fn read_aligned_byte(&mut self) -> Result<u8> {
        self.align_byte();
        let Some(byte) = self.bytes.get(self.byte_index).copied() else {
            return Err(git_error("Deflate stream ended unexpectedly"));
        };
        self.byte_index += 1;
        Ok(byte)
    }

    fn read_u16_le(&mut self) -> Result<u16> {
        let low = u16::from(self.read_aligned_byte()?);
        let high = u16::from(self.read_aligned_byte()?);
        Ok(low | (high << 8))
    }
}

const LENGTH_BASE: &[usize; 29] = &[
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: &[u8; 29] = &[
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: &[usize; 30] = &[
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: &[u8; 30] = &[
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: &[u8; 19] = &[
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn fixed_literal_huffman() -> Result<Huffman> {
    let mut lengths = vec![0_u8; 288];
    lengths[..=143].fill(8);
    lengths[144..=255].fill(9);
    lengths[256..=279].fill(7);
    lengths[280..=287].fill(8);
    Huffman::from_lengths(&lengths)
}

fn fixed_distance_huffman() -> Result<Huffman> {
    Huffman::from_lengths(&[5_u8; 32])
}

fn push_output(output: &mut Vec<u8>, byte: u8, max_output: usize) -> Result<()> {
    if output.len() >= max_output {
        return Err(git_error(format!(
            "Inflated git object exceeded {max_output} bytes"
        )));
    }
    output.push(byte);
    Ok(())
}

fn copy_from_output(
    output: &mut Vec<u8>,
    distance: usize,
    length: usize,
    max_output: usize,
) -> Result<()> {
    if distance == 0 || distance > output.len() {
        return Err(git_error("Invalid deflate back-reference distance"));
    }
    for _ in 0..length {
        let index = output.len() - distance;
        let byte = output[index];
        push_output(output, byte, max_output)?;
    }
    Ok(())
}

fn adler32(bytes: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65_521;
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in bytes {
        a = (a + u32::from(*byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    (b << 16) | a
}

fn verify_git_object_hash(
    oid: &str,
    object: &GitObject,
    object_format: GitObjectFormat,
) -> Result<()> {
    let expected = oid_hex_to_bytes(object_format, oid)?;
    let actual = git_object_hash(object_format, &object.kind, &object.data);
    if actual != expected {
        return Err(git_error(format!(
            "Git object {oid} hash mismatch: actual {}",
            bytes_to_hex(&actual)
        )));
    }
    Ok(())
}

fn git_object_hash(object_format: GitObjectFormat, kind: &str, data: &[u8]) -> Vec<u8> {
    match object_format {
        GitObjectFormat::Sha1 => {
            let mut sha1 = Sha1::new();
            update_git_object_hash(&mut sha1, kind, data);
            sha1.finalize().to_vec()
        }
        GitObjectFormat::Sha256 => {
            let mut sha256 = sha256::Sha256::new();
            update_git_object_hash(&mut sha256, kind, data);
            sha256.finalize().to_vec()
        }
    }
}

trait GitHash {
    fn update(&mut self, input: &[u8]);
}

impl GitHash for Sha1 {
    fn update(&mut self, input: &[u8]) {
        Sha1::update(self, input);
    }
}

impl GitHash for sha256::Sha256 {
    fn update(&mut self, input: &[u8]) {
        sha256::Sha256::update(self, input);
    }
}

fn update_git_object_hash(hash: &mut impl GitHash, kind: &str, data: &[u8]) {
    hash.update(kind.as_bytes());
    hash.update(b" ");
    hash.update(data.len().to_string().as_bytes());
    hash.update(&[0]);
    hash.update(data);
}

#[cfg(test)]
fn sha1_digest(bytes: &[u8]) -> [u8; GIT_OID_BYTES] {
    let mut sha1 = Sha1::new();
    sha1.update(bytes);
    sha1.finalize()
}

#[cfg(test)]
fn sha1_file_prefix(path: &Path, length: u64, action: &'static str) -> Result<[u8; GIT_OID_BYTES]> {
    let bytes = git_hash_file_prefix(GitObjectFormat::Sha1, path, length, action)?;
    let mut digest = [0_u8; GIT_OID_BYTES];
    digest.copy_from_slice(&bytes);
    Ok(digest)
}

fn git_hash_file_prefix(
    object_format: GitObjectFormat,
    path: &Path,
    length: u64,
    action: &'static str,
) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)
        .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 8192];
    match object_format {
        GitObjectFormat::Sha1 => {
            let mut sha1 = Sha1::new();
            hash_reader_prefix(
                &mut file,
                path,
                length,
                action,
                &mut remaining,
                &mut buffer,
                &mut sha1,
            )?;
            Ok(sha1.finalize().to_vec())
        }
        GitObjectFormat::Sha256 => {
            let mut sha256 = sha256::Sha256::new();
            hash_reader_prefix(
                &mut file,
                path,
                length,
                action,
                &mut remaining,
                &mut buffer,
                &mut sha256,
            )?;
            Ok(sha256.finalize().to_vec())
        }
    }
}

fn hash_reader_prefix(
    file: &mut fs::File,
    path: &Path,
    length: u64,
    action: &'static str,
    remaining: &mut u64,
    buffer: &mut [u8; 8192],
    hash: &mut impl GitHash,
) -> Result<()> {
    while *remaining > 0 {
        let requested = if *remaining > buffer.len() as u64 {
            buffer.len()
        } else {
            *remaining as usize
        };
        let count = file
            .read(&mut buffer[..requested])
            .map_err(|source| ArchivaError::io(Some(path.to_path_buf()), action, source))?;
        if count == 0 {
            return Err(git_error(format!(
                "{} ended before {length} bytes could be hashed",
                path.display()
            )));
        }
        hash.update(&buffer[..count]);
        *remaining -= count as u64;
    }
    Ok(())
}

struct Sha1 {
    state: [u32; 5],
    length_bytes: u64,
    buffer: [u8; 64],
    buffer_len: usize,
}

impl Sha1 {
    fn new() -> Self {
        Self {
            state: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            length_bytes: 0,
            buffer: [0; 64],
            buffer_len: 0,
        }
    }

    fn update(&mut self, mut input: &[u8]) {
        self.length_bytes = self
            .length_bytes
            .checked_add(input.len() as u64)
            .expect("SHA-1 input length overflow");
        if self.buffer_len > 0 {
            let needed = 64 - self.buffer_len;
            let copied = needed.min(input.len());
            self.buffer[self.buffer_len..self.buffer_len + copied]
                .copy_from_slice(&input[..copied]);
            self.buffer_len += copied;
            input = &input[copied..];
            if self.buffer_len == 64 {
                sha1_process_block(&mut self.state, &self.buffer);
                self.buffer_len = 0;
            }
        }
        while input.len() >= 64 {
            sha1_process_block(&mut self.state, &input[..64]);
            input = &input[64..];
        }
        if !input.is_empty() {
            self.buffer[..input.len()].copy_from_slice(input);
            self.buffer_len = input.len();
        }
    }

    fn finalize(mut self) -> [u8; GIT_SHA1_BYTES] {
        let bit_length = self
            .length_bytes
            .checked_mul(8)
            .expect("SHA-1 bit length overflow");
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;
        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..].fill(0);
            sha1_process_block(&mut self.state, &self.buffer);
            self.buffer_len = 0;
        }
        self.buffer[self.buffer_len..56].fill(0);
        self.buffer[56..64].copy_from_slice(&bit_length.to_be_bytes());
        sha1_process_block(&mut self.state, &self.buffer);

        let mut digest = [0_u8; GIT_SHA1_BYTES];
        for (index, word) in self.state.iter().enumerate() {
            digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

fn sha1_process_block(state: &mut [u32; 5], block: &[u8]) {
    let mut words = [0_u32; 80];
    for (index, chunk) in block.chunks_exact(4).take(16).enumerate() {
        words[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for index in 16..80 {
        words[index] =
            (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                .rotate_left(1);
    }

    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];

    for (index, word) in words.iter().enumerate() {
        let (function, constant) = match index {
            0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
            20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
            _ => (b ^ c ^ d, 0xca62_c1d6),
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(function)
            .wrapping_add(e)
            .wrapping_add(constant)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
}

fn reverse_bits(mut value: u16, len: u8) -> u16 {
    let mut reversed = 0_u16;
    for _ in 0..len {
        reversed = (reversed << 1) | (value & 1);
        value >>= 1;
    }
    reversed
}

fn validate_oid_hex(object_format: GitObjectFormat, oid: &str) -> Result<()> {
    if oid.len() != object_format.hex_len() || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(git_error(format!("Invalid git object id {oid:?}")));
    }
    Ok(())
}

fn oid_hex_to_bytes(object_format: GitObjectFormat, oid: &str) -> Result<Vec<u8>> {
    validate_oid_hex(object_format, oid)?;
    let mut bytes = vec![0_u8; object_format.raw_len()];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let start = index * 2;
        *byte = hex_pair_to_byte(&oid.as_bytes()[start..start + 2])?;
    }
    Ok(bytes)
}

fn hex_pair_to_byte(pair: &[u8]) -> Result<u8> {
    Ok((hex_value(pair[0])? << 4) | hex_value(pair[1])?)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(git_error(format!("Invalid git object id hex byte {byte}"))),
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn git_error(message: impl Into<String>) -> ArchivaError {
    ArchivaError::Git {
        message: message.into(),
    }
}

fn canonical_project_root(project_root: &Path) -> Result<PathBuf> {
    project_root.canonicalize().map_err(|source| {
        ArchivaError::io(
            Some(project_root.to_path_buf()),
            "resolve project root",
            source,
        )
    })
}

fn project_file_to_git_relative(
    project_root: &Path,
    git_root: &Path,
    file: &RelativePath,
) -> Result<String> {
    let absolute_source = project_root.join(file.to_path_buf());
    let relative_to_git =
        absolute_source
            .strip_prefix(git_root)
            .map_err(|_| ArchivaError::Git {
                message: format!("File {:?} is outside the git repository", file.as_str()),
            })?;
    path_to_forward_slashes(relative_to_git).ok_or_else(|| ArchivaError::Git {
        message: format!(
            "File {:?} is not valid UTF-8 relative to the git repository",
            file.as_str()
        ),
    })
}

fn path_to_forward_slashes(path: &Path) -> Option<String> {
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => segments.push(segment.to_str()?.to_string()),
            _ => return None,
        }
    }
    Some(segments.join("/"))
}

fn path_from_forward_slashes(path: &str) -> PathBuf {
    path.split('/').collect()
}

fn has_git_work_tree_marker(dir: &Path) -> bool {
    let marker = dir.join(".git");
    let metadata = match fs::metadata(&marker) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if metadata.is_dir() {
        return marker.join("HEAD").is_file();
    }
    if metadata.is_file() {
        return read_text_file_with_limit(&marker, GIT_MARKER_MAX_BYTES, "read git marker")
            .map(|content| {
                content
                    .lines()
                    .next()
                    .map(|line| line.trim_start().starts_with("gitdir:"))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        adler32, dynamic_distance_huffman, find_git_root, find_object_in_pack_index,
        has_git_work_tree_marker, oid_hex_to_bytes, pack_index_fanout_offset, pack_index_layout,
        pack_index_object_name_offset, pack_index_object_offset_field_offset, read_exact_at,
        read_git_head_file, read_git_head_file_native, read_git_object, read_pack_index_fanout,
        read_pack_index_format, read_pack_index_object_offset, read_pack_object_header,
        read_u32_be_at, sha1_digest, sha1_file_prefix, sorted_pack_index_offsets_once,
        validate_pack_index_fanout, zlib_inflate, GitObjectFormat, GitObjectReadContext, Huffman,
        PackIndexFormat, PackIndexLayout, GIT_OID_BYTES, GIT_PACK_INDEX_FANOUT_BYTES,
        GIT_PACK_INDEX_OFFSET_BYTES,
    };
    use crate::core::paths::RelativePath;
    use std::fs;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[test]
    fn returns_none_when_no_git_repository_exists() {
        let root = unique_temp_dir("archiva-git-none");
        fs::create_dir_all(&root).unwrap();
        assert_eq!(find_git_root(&root).unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_invalid_git_marker_directory() {
        let root = unique_temp_dir("archiva-git-invalid-marker");
        fs::create_dir_all(root.join(".git")).unwrap();
        assert!(!has_git_work_tree_marker(&root));
        assert_eq!(find_git_root(&root).unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_file_from_head_relative_to_git_root() {
        let root = unique_temp_dir("archiva-git-head");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        fs::write(root.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap()).unwrap(),
            "initial\n"
        );
        assert_eq!(
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_loose_head_blob_at_output_limit() {
        let root = unique_temp_dir("archiva-git-loose-output-limit");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        let content = "x".repeat(super::GIT_OUTPUT_MAX_BYTES);
        fs::write(root.join("pkg").join("src").join("large.ts"), &content).unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/large.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );

        let actual = read_git_head_file_native(
            &root.join("pkg"),
            &RelativePath::new("src/large.ts").unwrap(),
        )
        .unwrap();

        assert_eq!(actual.len(), super::GIT_OUTPUT_MAX_BYTES);
        assert!(actual.as_bytes().iter().all(|byte| *byte == b'x'));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_git_object_format_before_head_resolution() {
        let root = unique_temp_dir("archiva-git-unknown-format");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        fs::write(
            root.join(".git").join("config"),
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tobjectFormat = blake3\n",
        )
        .unwrap();

        let error =
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap_err()
                .user_message();
        assert!(error.contains("unsupported"));
        assert!(error.contains("blake3"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_file_from_sha256_loose_head() {
        let root = unique_temp_dir("archiva-git-sha256-head");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init", "--object-format=sha256"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        assert_eq!(git_output(&root, &["rev-parse", "HEAD"]).trim().len(), 64);
        fs::write(root.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_file_from_sha256_packed_head() {
        let root = unique_temp_dir("archiva-git-sha256-packed-head");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init", "--object-format=sha256"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        fs::write(root.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_file_from_packed_head_without_git_show() {
        let root = unique_temp_dir("archiva-git-packed-head");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&root, &["repack", "-ad"]);
        fs::remove_dir_all(root.join(".git").join("objects").join("06")).ok();
        fs::write(root.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_packed_incompressible_blob_at_output_limit() {
        let root = unique_temp_dir("archiva-git-packed-output-limit");
        fs::create_dir_all(root.join("src")).unwrap();
        let content = incompressible_bytes(super::GIT_OUTPUT_MAX_BYTES);
        fs::write(root.join("src").join("large.bin"), &content).unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/large.bin"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/large.bin"])
            .trim()
            .to_string();
        git(&root, &["repack", "-adf"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        assert!(
            packed_object_compressed_range(&idx_path, &target_oid)
                > super::GIT_OUTPUT_MAX_BYTES as u64
        );

        let actual = read_git_object(&root.join(".git"), &target_oid).unwrap();

        assert_eq!(actual.kind, "blob");
        assert_eq!(actual.data.len(), content.len());
        assert_eq!(sha1_digest(&actual.data), sha1_digest(&content));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_file_from_v1_pack_index() {
        let root = unique_temp_dir("archiva-git-v1-pack-index");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&root, &["-c", "pack.indexVersion=1", "repack", "-ad"]);
        git(&root, &["prune-packed"]);
        assert_pack_index_format(&single_pack_index(&root), PackIndexFormat::V1);
        fs::write(root.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn caches_sorted_pack_index_offsets_for_next_object_lookup() {
        let root = unique_temp_dir("archiva-git-pack-offset-cache");
        fs::create_dir_all(root.join("src")).unwrap();
        for index in 0..8 {
            fs::write(
                root.join("src").join(format!("file-{index}.ts")),
                format!("export const value{index} = {index};\n"),
            )
            .unwrap();
        }
        git(&root, &["init"]);
        git(&root, &["add", "src"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        let mut index_file = fs::File::open(&idx_path).unwrap();
        let format = read_pack_index_format(&mut index_file, &idx_path).unwrap();
        let fanout = read_pack_index_fanout(&mut index_file, &idx_path, format).unwrap();
        let layout =
            pack_index_layout(&idx_path, format, fanout[255], GitObjectFormat::Sha1).unwrap();
        let pack_data_end = fs::metadata(idx_path.with_extension("pack")).unwrap().len()
            - super::GIT_PACK_TRAILER_BYTES;
        let mut context = GitObjectReadContext::default();

        let cached_offsets = {
            let offsets =
                sorted_pack_index_offsets_once(&mut context, &mut index_file, &idx_path, layout)
                    .unwrap();
            assert!(offsets.windows(2).all(|pair| pair[0] <= pair[1]));
            offsets.to_vec()
        };
        assert_eq!(context.pack_index_offsets.len(), 1);
        let second =
            sorted_pack_index_offsets_once(&mut context, &mut index_file, &idx_path, layout)
                .unwrap()
                .to_vec();
        assert_eq!(second, cached_offsets);
        assert_eq!(context.pack_index_offsets.len(), 1);

        for (position, offset) in cached_offsets.iter().enumerate() {
            let expected = cached_offsets
                .get(position + 1)
                .copied()
                .unwrap_or(pack_data_end);
            assert_eq!(
                super::next_pack_object_offset(
                    &mut context,
                    &mut index_file,
                    &idx_path,
                    layout,
                    *offset,
                    pack_data_end,
                )
                .unwrap(),
                expected
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_deltified_blob_from_pack() {
        let root = unique_temp_dir("archiva-git-packed-delta");
        fs::create_dir_all(root.join("src")).unwrap();
        let common = "let shared = \"".to_string() + &"x".repeat(4096) + "\";\n";
        for index in 0..30 {
            fs::write(
                root.join("src").join(format!("file-{index:02}.ts")),
                format!("{common}export const value{index} = {index};\n"),
            )
            .unwrap();
        }
        git(&root, &["init"]);
        git(&root, &["add", "src"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&root, &["repack", "-adf", "--window=50", "--depth=50"]);
        let idx_path = fs::read_dir(root.join(".git").join("objects").join("pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("idx"))
            .unwrap();
        let verify = git_output(&root, &["verify-pack", "-v", idx_path.to_str().unwrap()]);
        let delta_oid = verify
            .lines()
            .filter_map(|line| {
                let parts = line.split_whitespace().collect::<Vec<_>>();
                (parts.len() >= 7 && parts[1] == "blob").then_some(parts[0].to_string())
            })
            .next()
            .expect("expected git to create a deltified blob");
        let expected = git_output(&root, &["cat-file", "-p", &delta_oid]);
        let actual = read_git_object(&root.join(".git"), &delta_oid).unwrap();

        assert_eq!(actual.kind, "blob");
        assert_eq!(String::from_utf8(actual.data).unwrap(), expected);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_deltified_blob_from_v1_pack_index() {
        let root = unique_temp_dir("archiva-git-packed-delta-v1-index");
        fs::create_dir_all(root.join("src")).unwrap();
        let common = "let shared = \"".to_string() + &"x".repeat(4096) + "\";\n";
        for index in 0..30 {
            fs::write(
                root.join("src").join(format!("file-{index:02}.ts")),
                format!("{common}export const value{index} = {index};\n"),
            )
            .unwrap();
        }
        git(&root, &["init"]);
        git(&root, &["add", "src"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(
            &root,
            &[
                "-c",
                "pack.indexVersion=1",
                "repack",
                "-adf",
                "--window=50",
                "--depth=50",
            ],
        );
        let idx_path = single_pack_index(&root);
        assert_pack_index_format(&idx_path, PackIndexFormat::V1);
        let delta_oid = ofs_delta_blob_oid(&root, &idx_path);
        let expected = git_output(&root, &["cat-file", "-p", &delta_oid]);
        let actual = read_git_object(&root.join(".git"), &delta_oid).unwrap();

        assert_eq!(actual.kind, "blob");
        assert_eq!(String::from_utf8(actual.data).unwrap(), expected);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_loose_object_when_rebuilt_hash_differs_from_oid() {
        let root = unique_temp_dir("archiva-git-loose-hash-mismatch");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let blob_oid = git_output(&root, &["rev-parse", "HEAD:pkg/src/a.ts"])
            .trim()
            .to_string();
        let replacement = b"changed\n";
        let mut inflated = format!("blob {}\0", replacement.len()).into_bytes();
        inflated.extend_from_slice(replacement);
        let object_path = root
            .join(".git")
            .join("objects")
            .join(&blob_oid[..2])
            .join(&blob_oid[2..]);
        make_writable(&object_path);
        fs::write(&object_path, zlib_stored_stream(&inflated)).unwrap();

        let error =
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap_err()
                .user_message();
        assert!(error.contains("hash mismatch"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_index_points_to_wrong_offset() {
        let root = unique_temp_dir("archiva-git-pack-hash-mismatch");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        fs::write(root.join("src").join("b.ts"), "bravo\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        let replacement_oid = git_output(&root, &["rev-parse", "HEAD:src/b.ts"])
            .trim()
            .to_string();
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        fs::remove_file(
            root.join(".git")
                .join("objects")
                .join(&target_oid[..2])
                .join(&target_oid[2..]),
        )
        .ok();
        let idx_path = fs::read_dir(root.join(".git").join("objects").join("pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("idx"))
            .unwrap();
        rewrite_pack_index_offset(&idx_path, &target_oid, &replacement_oid);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("hash mismatch"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_index_checksum_is_corrupt() {
        let root = unique_temp_dir("archiva-git-pack-index-checksum");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        flip_last_byte(&idx_path);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("pack index"));
        assert!(error.contains("checksum mismatch"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_absent_prefix_pack_index_when_checksum_is_corrupt() {
        let root = unique_temp_dir("archiva-git-pack-index-absent-checksum");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        let absent_oid = absent_pack_index_oid(&idx_path);
        flip_last_byte(&idx_path);

        let error = match find_object_in_pack_index(
            &idx_path,
            &absent_oid,
            GitObjectFormat::Sha1,
            &mut GitObjectReadContext::default(),
        ) {
            Ok(_) => panic!("expected corrupt pack index checksum to be rejected"),
            Err(error) => error.user_message(),
        };
        assert!(error.contains("pack index"));
        assert!(error.contains("checksum mismatch"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_absent_prefix_pack_index_when_fanout_names_are_inconsistent() {
        let root = unique_temp_dir("archiva-git-pack-index-absent-fanout");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        fs::write(root.join("src").join("b.ts"), "bravo\n").unwrap();
        fs::write(root.join("src").join("c.ts"), "charlie\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        let target_oid = first_pack_index_oid_before_bucket(&idx_path, 255);
        hide_pack_index_fanout_bucket(&idx_path, target_oid[0]);

        let error = match find_object_in_pack_index(
            &idx_path,
            &target_oid,
            GitObjectFormat::Sha1,
            &mut GitObjectReadContext::default(),
        ) {
            Ok(_) => panic!("expected malformed pack index fanout to be rejected"),
            Err(error) => error.user_message(),
        };
        assert!(error.contains("fanout does not match object id table"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_packed_object_after_valid_absent_prefix_miss_in_prior_index() {
        let prior_root = unique_temp_dir("archiva-git-pack-prior-miss");
        fs::create_dir_all(prior_root.join("src")).unwrap();
        fs::write(prior_root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&prior_root, &["init"]);
        git(&prior_root, &["add", "src/a.ts"]);
        git(
            &prior_root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&prior_root, &["repack", "-ad"]);
        git(&prior_root, &["prune-packed"]);
        let prior_idx_path = single_pack_index(&prior_root);
        let target_prefix = absent_pack_index_oid(&prior_idx_path)[0];
        let (target_data, target_oid) = blob_fixture_for_prefix(target_prefix);

        let target_root = unique_temp_dir("archiva-git-pack-later-hit");
        fs::create_dir_all(target_root.join("src")).unwrap();
        fs::write(target_root.join("src").join("b.ts"), target_data).unwrap();
        git(&target_root, &["init"]);
        git(&target_root, &["add", "src/b.ts"]);
        git(
            &target_root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(&target_root, &["repack", "-ad"]);
        git(&target_root, &["prune-packed"]);
        let target_idx_path = single_pack_index(&target_root);
        let mut context = GitObjectReadContext::default();

        assert!(find_object_in_pack_index(
            &prior_idx_path,
            &target_oid,
            GitObjectFormat::Sha1,
            &mut context,
        )
        .unwrap()
        .is_none());
        assert!(find_object_in_pack_index(
            &target_idx_path,
            &target_oid,
            GitObjectFormat::Sha1,
            &mut context,
        )
        .unwrap()
        .is_some());

        let _ = fs::remove_dir_all(prior_root);
        let _ = fs::remove_dir_all(target_root);
    }

    #[test]
    fn rejects_packed_object_when_index_object_names_are_unsorted() {
        let root = unique_temp_dir("archiva-git-pack-index-unsorted-names");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        fs::write(root.join("src").join("b.ts"), "bravo\n").unwrap();
        fs::write(root.join("src").join("c.ts"), "charlie\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        swap_first_two_pack_index_names(&idx_path);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("object ids are not sorted"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_v1_index_layout_is_malformed() {
        let root = unique_temp_dir("archiva-git-pack-index-v1-layout");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["-c", "pack.indexVersion=1", "repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        assert_pack_index_format(&idx_path, PackIndexFormat::V1);
        increment_v1_pack_index_object_count(&idx_path);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("v1 sections"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_v1_index_checksum_is_corrupt() {
        let root = unique_temp_dir("archiva-git-pack-index-v1-checksum");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["-c", "pack.indexVersion=1", "repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        assert_pack_index_format(&idx_path, PackIndexFormat::V1);
        flip_last_byte(&idx_path);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("pack index"));
        assert!(error.contains("checksum mismatch"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_index_stored_pack_checksum_is_corrupt() {
        let root = unique_temp_dir("archiva-git-pack-index-stored-pack-checksum");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        let stored_pack_checksum_offset = fs::metadata(&idx_path).unwrap().len() - 40;
        flip_byte_at(&idx_path, stored_pack_checksum_offset);
        rewrite_pack_index_checksum(&idx_path);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("checksum does not match index"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_large_offset_table_reference_is_out_of_bounds() {
        let root = unique_temp_dir("archiva-git-pack-index-large-offset");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        rewrite_pack_index_offset_to_large_reference(&idx_path, &target_oid, 0);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("large offset table"));
        assert!(error.contains("exceeds 0 entries"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_pack_trailer_is_corrupt() {
        let root = unique_temp_dir("archiva-git-pack-trailer-checksum");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let pack_path = single_pack_index(&root).with_extension("pack");
        flip_last_byte(&pack_path);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("trailer checksum mismatch"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_packed_object_when_pack_header_count_mismatches_index() {
        let root = unique_temp_dir("archiva-git-pack-header-count");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("a.ts"), "alpha\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let target_oid = git_output(&root, &["rev-parse", "HEAD:src/a.ts"])
            .trim()
            .to_string();
        git(&root, &["repack", "-ad"]);
        git(&root, &["prune-packed"]);
        let idx_path = single_pack_index(&root);
        rewrite_pack_header_object_count(&idx_path, 0);

        let error = read_git_object(&root.join(".git"), &target_oid)
            .unwrap_err()
            .user_message();
        assert!(error.contains("object count"));
        assert!(error.contains("does not match index count"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_head_file_from_linked_worktree_common_dir() {
        let root = unique_temp_dir("archiva-git-linked-root");
        let linked = unique_temp_dir("archiva-git-linked-worktree");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "archiva-linked-test",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(linked.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file_native(&linked.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(linked);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_sha256_head_file_from_linked_worktree_common_dir() {
        let root = unique_temp_dir("archiva-git-sha256-linked-root");
        let linked = unique_temp_dir("archiva-git-sha256-linked-worktree");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init", "--object-format=sha256"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "archiva-sha256-linked-test",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(linked.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file_native(&linked.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(linked);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_cyclic_symbolic_head_refs_at_depth_limit() {
        let root = unique_temp_dir("archiva-git-symbolic-ref-cycle");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        fs::write(
            root.join(".git").join("refs").join("heads").join("cycle-a"),
            "ref: refs/heads/cycle-b\n",
        )
        .unwrap();
        fs::write(
            root.join(".git").join("refs").join("heads").join("cycle-b"),
            "ref: refs/heads/cycle-a\n",
        )
        .unwrap();
        fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/cycle-a\n").unwrap();

        let error =
            read_git_head_file_native(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap())
                .unwrap_err()
                .user_message();

        assert!(error.contains("symbolic ref chain exceeded maximum depth"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_head_file_from_alternate_object_store() {
        let source = unique_temp_dir("archiva-git-alternate-source");
        let borrower = unique_temp_dir("archiva-git-alternate-borrower");
        fs::create_dir_all(source.join("pkg").join("src")).unwrap();
        fs::write(source.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&source, &["init"]);
        git(&source, &["add", "pkg/src/a.ts"]);
        git(
            &source,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        let head_oid = git_output(&source, &["rev-parse", "HEAD"]);
        fs::create_dir_all(borrower.join("pkg").join("src")).unwrap();
        fs::write(borrower.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();
        git(&borrower, &["init"]);
        let head = fs::read_to_string(borrower.join(".git").join("HEAD")).unwrap();
        let reference = head.trim().strip_prefix("ref: ").unwrap();
        fs::create_dir_all(borrower.join(".git").join("objects").join("info")).unwrap();
        fs::create_dir_all(borrower.join(".git").join(reference).parent().unwrap()).unwrap();
        fs::write(
            borrower.join(".git").join(reference),
            format!("{}\n", head_oid.trim()),
        )
        .unwrap();
        fs::write(
            borrower
                .join(".git")
                .join("objects")
                .join("info")
                .join("alternates"),
            source.join(".git").join("objects").to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_git_head_file_native(
                &borrower.join("pkg"),
                &RelativePath::new("src/a.ts").unwrap()
            )
            .unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(borrower);
    }

    #[test]
    fn parses_commit_tree_from_non_utf8_commit_message() {
        let tree = "0123456789abcdef0123456789abcdef01234567";
        let mut commit =
            format!("tree {tree}\nauthor A <a@example.invalid> 0 +0000\n").into_bytes();
        commit.extend_from_slice(b"\nmessage with invalid byte ");
        commit.push(0xff);

        assert_eq!(
            super::commit_tree_oid(&commit, GitObjectFormat::Sha1).unwrap(),
            tree
        );
    }

    #[test]
    fn zlib_inflate_verifies_adler32_checksum() {
        let valid = zlib_stored_stream(b"hi");
        assert_eq!(zlib_inflate(&valid, 16).unwrap(), b"hi");

        let mut corrupt = valid;
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;

        let error = zlib_inflate(&corrupt, 16).unwrap_err().user_message();
        assert!(error.contains("Adler-32 checksum mismatch"));
    }

    #[test]
    fn sha1_digest_matches_known_vector() {
        assert_eq!(
            super::bytes_to_hex(&sha1_digest(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn zlib_inflate_rejects_trailing_deflate_bytes_before_checksum() {
        let mut stream = zlib_stored_stream(b"hi");
        let trailer = stream.split_off(stream.len() - 4);
        stream.push(0);
        stream.extend_from_slice(&trailer);

        let error = zlib_inflate(&stream, 16).unwrap_err().user_message();
        assert!(error.contains("trailing deflate bytes"));
    }

    #[test]
    fn rejects_oversubscribed_huffman_tables() {
        let error = Huffman::from_lengths(&[1, 1, 1])
            .unwrap_err()
            .user_message();
        assert!(error.contains("oversubscribed"));
    }

    #[test]
    fn dynamic_distance_huffman_allows_literal_only_empty_distance_table() {
        let literal = literal_huffman_with_symbols(&[65, 256]);
        let distance = dynamic_distance_huffman(&literal, &[0]).unwrap();
        assert_eq!(distance.entries.len(), 0);
    }

    #[test]
    fn dynamic_distance_huffman_rejects_empty_distance_table_with_length_symbols() {
        let literal = literal_huffman_with_symbols(&[256, 257]);
        let error = dynamic_distance_huffman(&literal, &[0])
            .unwrap_err()
            .user_message();
        assert!(error.contains("require a distance table"));
    }

    fn git(root: &PathBuf, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}\nstdout={}\nstderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(root: &PathBuf, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}\nstdout={}\nstderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn zlib_stored_stream(data: &[u8]) -> Vec<u8> {
        assert!(data.len() <= u16::MAX as usize);
        let len = data.len() as u16;
        let mut stream = vec![0x78, 0x01, 0x01];
        stream.extend_from_slice(&len.to_le_bytes());
        stream.extend_from_slice(&(!len).to_le_bytes());
        stream.extend_from_slice(data);
        stream.extend_from_slice(&adler32(data).to_be_bytes());
        stream
    }

    fn literal_huffman_with_symbols(symbols: &[usize]) -> Huffman {
        let mut lengths = vec![0_u8; 286];
        for symbol in symbols {
            lengths[*symbol] = 1;
        }
        Huffman::from_lengths(&lengths).unwrap()
    }

    fn rewrite_pack_index_offset(idx_path: &PathBuf, target_oid: &str, replacement_oid: &str) {
        let (target_index, layout, _) = pack_index_entry(idx_path, target_oid);
        let (_, _, replacement_offset) = pack_index_entry(idx_path, replacement_oid);
        assert_eq!(
            layout.format,
            PackIndexFormat::V2,
            "large-offset rewrite fixture expects a v2 pack index"
        );
        assert!(
            replacement_offset <= u64::from(u32::MAX >> 1),
            "fixture pack unexpectedly used a large offset"
        );
        make_writable(idx_path);
        let mut index = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(idx_path)
            .unwrap();
        index
            .seek(SeekFrom::Start(pack_index_object_offset_field_offset(
                layout,
                target_index,
            )))
            .unwrap();
        index
            .write_all(&(replacement_offset as u32).to_be_bytes())
            .unwrap();
        index.flush().unwrap();
        rewrite_pack_index_checksum(idx_path);
    }

    fn rewrite_pack_index_offset_to_large_reference(
        idx_path: &PathBuf,
        target_oid: &str,
        large_index: u32,
    ) {
        let (target_index, layout, _) = pack_index_entry(idx_path, target_oid);
        assert_eq!(
            layout.format,
            PackIndexFormat::V2,
            "large-offset rewrite fixture expects a v2 pack index"
        );
        assert!(
            large_index <= 0x7fff_ffff,
            "large offset table reference index exceeds encodable range"
        );
        make_writable(idx_path);
        let mut index = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(idx_path)
            .unwrap();
        index
            .seek(SeekFrom::Start(pack_index_object_offset_field_offset(
                layout,
                target_index,
            )))
            .unwrap();
        index
            .write_all(&(0x8000_0000 | large_index).to_be_bytes())
            .unwrap();
        index.flush().unwrap();
        rewrite_pack_index_checksum(idx_path);
    }

    fn make_writable(path: &PathBuf) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | 0o200);
        }
        #[cfg(not(unix))]
        {
            permissions.set_readonly(false);
        }
        fs::set_permissions(path, permissions).unwrap();
    }

    fn rewrite_pack_index_checksum(idx_path: &PathBuf) {
        let len = fs::metadata(idx_path).unwrap().len();
        let checksum = sha1_file_prefix(idx_path, len - 20, "hash git pack index").unwrap();
        let mut index = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(idx_path)
            .unwrap();
        index.seek(SeekFrom::Start(len - 20)).unwrap();
        index.write_all(&checksum).unwrap();
    }

    fn rewrite_pack_header_object_count(idx_path: &PathBuf, object_count: u32) {
        let pack_path = idx_path.with_extension("pack");
        make_writable(&pack_path);
        let mut pack = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pack_path)
            .unwrap();
        pack.seek(SeekFrom::Start(8)).unwrap();
        pack.write_all(&object_count.to_be_bytes()).unwrap();
        pack.flush().unwrap();
        rewrite_pack_trailer_and_index_pack_checksum(idx_path);
    }

    fn rewrite_pack_trailer_and_index_pack_checksum(idx_path: &PathBuf) {
        let pack_path = idx_path.with_extension("pack");
        let pack_len = fs::metadata(&pack_path).unwrap().len();
        let checksum = sha1_file_prefix(&pack_path, pack_len - 20, "hash git pack").unwrap();
        let mut pack = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pack_path)
            .unwrap();
        pack.seek(SeekFrom::Start(pack_len - 20)).unwrap();
        pack.write_all(&checksum).unwrap();
        pack.flush().unwrap();

        make_writable(idx_path);
        let index_len = fs::metadata(idx_path).unwrap().len();
        let mut index = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(idx_path)
            .unwrap();
        index.seek(SeekFrom::Start(index_len - 40)).unwrap();
        index.write_all(&checksum).unwrap();
        index.flush().unwrap();
        rewrite_pack_index_checksum(idx_path);
    }

    fn flip_last_byte(path: &PathBuf) {
        let len = fs::metadata(path).unwrap().len();
        flip_byte_at(path, len - 1);
    }

    fn flip_byte_at(path: &PathBuf, offset: u64) {
        make_writable(path);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 1;
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&byte).unwrap();
    }

    fn single_pack_index(root: &Path) -> PathBuf {
        fs::read_dir(root.join(".git").join("objects").join("pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("idx"))
            .unwrap()
    }

    fn absent_pack_index_oid(idx_path: &PathBuf) -> [u8; GIT_OID_BYTES] {
        let mut index = fs::File::open(idx_path).unwrap();
        let format = read_pack_index_format(&mut index, idx_path).unwrap();
        let fanout = read_pack_index_fanout(&mut index, idx_path, format).unwrap();
        let mut previous = 0_u32;
        for (prefix, count) in fanout.iter().enumerate() {
            if *count == previous {
                let mut oid = [0_u8; GIT_OID_BYTES];
                oid[0] = prefix as u8;
                return oid;
            }
            previous = *count;
        }
        panic!("expected fixture pack index to have an absent first-byte prefix");
    }

    fn first_pack_index_oid_before_bucket(
        idx_path: &PathBuf,
        max_prefix: u8,
    ) -> [u8; GIT_OID_BYTES] {
        let mut index = fs::File::open(idx_path).unwrap();
        let format = read_pack_index_format(&mut index, idx_path).unwrap();
        let fanout = read_pack_index_fanout(&mut index, idx_path, format).unwrap();
        let layout =
            pack_index_layout(idx_path, format, fanout[255], GitObjectFormat::Sha1).unwrap();
        for object_index in 0..layout.object_count {
            let mut oid = [0_u8; GIT_OID_BYTES];
            read_exact_at(
                &mut index,
                idx_path,
                pack_index_object_name_offset(layout, object_index),
                &mut oid,
                "read git pack index object id",
            )
            .unwrap();
            if oid[0] < max_prefix {
                return oid;
            }
        }
        panic!("expected fixture pack index to contain an object below prefix {max_prefix}");
    }

    fn hide_pack_index_fanout_bucket(idx_path: &PathBuf, prefix: u8) {
        let prefix = usize::from(prefix);
        assert!(
            prefix < 255,
            "fixture keeps the final object count unchanged"
        );
        make_writable(idx_path);
        let mut index = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(idx_path)
            .unwrap();
        let format = read_pack_index_format(&mut index, idx_path).unwrap();
        let fanout = read_pack_index_fanout(&mut index, idx_path, format).unwrap();
        let previous = if prefix == 0 { 0 } else { fanout[prefix - 1] };
        assert!(fanout[prefix] > previous);
        let offset = pack_index_fanout_offset(format)
            + u64::try_from(prefix).unwrap() * GIT_PACK_INDEX_OFFSET_BYTES;
        index.seek(SeekFrom::Start(offset)).unwrap();
        index.write_all(&previous.to_be_bytes()).unwrap();
        index.flush().unwrap();
        rewrite_pack_index_checksum(idx_path);
    }

    fn blob_fixture_for_prefix(prefix: u8) -> (Vec<u8>, [u8; GIT_OID_BYTES]) {
        for counter in 0..10_000 {
            let data = format!("bravo-{counter}\n").into_bytes();
            let mut object = format!("blob {}\0", data.len()).into_bytes();
            object.extend_from_slice(&data);
            let oid = sha1_digest(&object);
            if oid[0] == prefix {
                return (data, oid);
            }
        }
        panic!("expected to find blob fixture for prefix {prefix:02x}");
    }

    fn incompressible_bytes(len: usize) -> Vec<u8> {
        let mut state = 0x8f43_2a1b_9e37_79c5_u64;
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.push((state >> 32) as u8);
        }
        bytes
    }

    fn packed_object_compressed_range(idx_path: &PathBuf, oid: &str) -> u64 {
        let (_, layout, offset) = pack_index_entry(idx_path, oid);
        let mut index = fs::File::open(idx_path).unwrap();
        let mut context = GitObjectReadContext::default();
        let pack_path = idx_path.with_extension("pack");
        let pack_data_end = fs::metadata(&pack_path).unwrap().len() - super::GIT_PACK_TRAILER_BYTES;
        let next_offset = super::next_pack_object_offset(
            &mut context,
            &mut index,
            idx_path,
            layout,
            offset,
            pack_data_end,
        )
        .unwrap();
        let mut pack = fs::File::open(&pack_path).unwrap();
        pack.seek(SeekFrom::Start(offset)).unwrap();
        let (_, _) = read_pack_object_header(&mut pack, &pack_path).unwrap();
        next_offset - pack.stream_position().unwrap()
    }

    fn ofs_delta_blob_oid(root: &PathBuf, idx_path: &Path) -> String {
        let verify = git_output(root, &["verify-pack", "-v", idx_path.to_str().unwrap()]);
        let pack_path = idx_path.with_extension("pack");
        for line in verify.lines() {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.len() < 7 || parts[1] != "blob" {
                continue;
            }
            let Ok(offset) = parts[4].parse::<u64>() else {
                continue;
            };
            let mut pack = fs::File::open(&pack_path).unwrap();
            pack.seek(SeekFrom::Start(offset)).unwrap();
            let (object_type, _) = read_pack_object_header(&mut pack, &pack_path).unwrap();
            if object_type == 6 {
                return parts[0].to_string();
            }
        }
        panic!("expected git to create an OFS_DELTA blob");
    }

    fn assert_pack_index_format(idx_path: &PathBuf, expected: PackIndexFormat) {
        let mut index = fs::File::open(idx_path).unwrap();
        let actual = read_pack_index_format(&mut index, idx_path).unwrap();
        assert_eq!(actual, expected);
    }

    fn increment_v1_pack_index_object_count(idx_path: &PathBuf) {
        make_writable(idx_path);
        let mut index = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(idx_path)
            .unwrap();
        let format = read_pack_index_format(&mut index, idx_path).unwrap();
        assert_eq!(format, PackIndexFormat::V1);
        let count_offset = GIT_PACK_INDEX_FANOUT_BYTES - GIT_PACK_INDEX_OFFSET_BYTES;
        let count = read_u32_be_at(
            &mut index,
            idx_path,
            count_offset,
            "read git pack index fanout",
        )
        .unwrap();
        index.seek(SeekFrom::Start(count_offset)).unwrap();
        index.write_all(&(count + 1).to_be_bytes()).unwrap();
        index.flush().unwrap();
        rewrite_pack_index_checksum(idx_path);
    }

    fn swap_first_two_pack_index_names(idx_path: &PathBuf) {
        make_writable(idx_path);
        let mut index = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(idx_path)
            .unwrap();
        let format = read_pack_index_format(&mut index, idx_path).unwrap();
        let fanout = read_pack_index_fanout(&mut index, idx_path, format).unwrap();
        let layout =
            pack_index_layout(idx_path, format, fanout[255], GitObjectFormat::Sha1).unwrap();
        assert!(layout.object_count >= 2);
        let first_offset = pack_index_object_name_offset(layout, 0);
        let second_offset = pack_index_object_name_offset(layout, 1);
        let mut first = [0_u8; GIT_OID_BYTES];
        let mut second = [0_u8; GIT_OID_BYTES];
        read_exact_at(
            &mut index,
            idx_path,
            first_offset,
            &mut first,
            "read git pack index object id",
        )
        .unwrap();
        read_exact_at(
            &mut index,
            idx_path,
            second_offset,
            &mut second,
            "read git pack index object id",
        )
        .unwrap();
        assert!(first < second);
        index.seek(SeekFrom::Start(first_offset)).unwrap();
        index.write_all(&second).unwrap();
        index.seek(SeekFrom::Start(second_offset)).unwrap();
        index.write_all(&first).unwrap();
        index.flush().unwrap();
        rewrite_pack_index_checksum(idx_path);
    }

    fn pack_index_entry(idx_path: &PathBuf, oid: &str) -> (u32, PackIndexLayout, u64) {
        let oid = oid_hex_to_bytes(GitObjectFormat::Sha1, oid).unwrap();
        let mut index = fs::File::open(idx_path).unwrap();
        let format = read_pack_index_format(&mut index, idx_path).unwrap();
        let fanout = read_pack_index_fanout(&mut index, idx_path, format).unwrap();
        validate_pack_index_fanout(idx_path, &fanout).unwrap();
        let layout =
            pack_index_layout(idx_path, format, fanout[255], GitObjectFormat::Sha1).unwrap();
        for object_index in 0..layout.object_count {
            let mut candidate = [0_u8; 20];
            read_exact_at(
                &mut index,
                idx_path,
                pack_index_object_name_offset(layout, object_index),
                &mut candidate,
                "read git pack index object id",
            )
            .unwrap();
            if candidate.as_slice() == oid.as_slice() {
                let offset =
                    read_pack_index_object_offset(&mut index, idx_path, layout, object_index)
                        .unwrap();
                return (object_index, layout, offset);
            }
        }
        panic!("object id {oid:?} not found in pack index");
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
        path
    }
}
