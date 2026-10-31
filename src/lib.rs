use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const BUFFER_CAPACITY: usize = 128 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarZstd,
    Zstd,
    SevenZ,
    TarXz,
    Xz,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        if name.ends_with(".tar.zst") || name.ends_with(".tar.zstd") {
            Some(Self::TarZstd)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some(Self::TarXz)
        } else if name.ends_with(".7z") {
            Some(Self::SevenZ)
        } else if name.ends_with(".tar") {
            Some(Self::Tar)
        } else if name.ends_with(".zip") {
            Some(Self::Zip)
        } else if name.ends_with(".zst") || name.ends_with(".zstd") {
            Some(Self::Zstd)
        } else if name.ends_with(".xz") {
            Some(Self::Xz)
        } else {
            None
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarZstd => "tar.zst",
            Self::Zstd => "zst",
            Self::SevenZ => "7z",
            Self::TarXz => "tar.xz",
            Self::Xz => "xz",
        }
    }
}

pub struct Entry {
    pub path: String,
    pub size: u64,
    pub compressed: Option<u64>,
    pub is_dir: bool,
}

pub fn format_size(bytes: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = K * 1024;
    const G: u64 = M * 1024;
    if bytes >= G {
        format!("{:.2} GB", bytes as f64 / G as f64)
    } else if bytes >= M {
        format!("{:.2} MB", bytes as f64 / M as f64)
    } else if bytes >= K {
        format!("{:.1} KB", bytes as f64 / K as f64)
    } else {
        format!("{bytes} B")
    }
}

fn compression_level(format: Format, level: i32) -> u32 {
    let level = level.clamp(0, 19) as u32;
    match format {
        Format::Zstd | Format::TarZstd => level.clamp(1, 19),
        Format::Zip | Format::TarGz | Format::Xz | Format::TarXz => {
            ((level as f64 / 19.0) * 9.0).round() as u32
        }
        _ => 5,
    }
}

fn collect_files(paths: &[&Path]) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for path in paths {
        let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Cannot access: {}", path.display()))?;

        if metadata.file_type().is_symlink() {
            bail!("Symlinks are not supported as inputs: {}", path.display());
        }

        if metadata.is_file() {
            let canonical = path.canonicalize()
            .with_context(|| format!("Cannot resolve: {}", path.display()))?;
            if seen.insert(canonical.clone()) {
                let name = path.file_name().context("Invalid file name")?;
                result.push((canonical, PathBuf::from(name)));
            }
        } else if metadata.is_dir() {
            let root = path.canonicalize()
            .with_context(|| format!("Cannot resolve: {}", path.display()))?;
            let base = root.parent().unwrap_or(&root);

            for item in WalkDir::new(&root).follow_links(false) {
                let entry = item.with_context(|| format!("Failed to scan: {}", path.display()))?;
                if entry.file_type().is_symlink() || !entry.file_type().is_file() {
                    continue;
                }

                let full = entry.path().canonicalize()
                .with_context(|| format!("Cannot resolve: {}", entry.path().display()))?;
                if !seen.insert(full.clone()) {
                    continue;
                }

                let relative = full.strip_prefix(base)
                .context("Failed to create archive-relative path")?
                .to_path_buf();
                result.push((full, relative));
            }
        } else {
            bail!("Unsupported input type: {}", path.display());
        }
    }

    Ok(result)
}

fn ensure_output_not_input(files: &[(PathBuf, PathBuf)], output: &Path) -> Result<()> {
    if output.exists() {
        let out = output.canonicalize()
        .with_context(|| format!("Cannot resolve output: {}", output.display()))?;
        if files.iter().any(|(src, _)| *src == out) {
            bail!("Output archive cannot overwrite one of its input files");
        }
    } else if let (Some(parent), Some(name)) = (output.parent(), output.file_name()) {
        let parent = parent.canonicalize()
        .with_context(|| format!("Cannot resolve output directory: {}", parent.display()))?;
        let candidate = parent.join(name);
        if files.iter().any(|(src, _)| *src == candidate) {
            bail!("Output archive conflicts with an input file");
        }
    }
    Ok(())
}

pub fn compress<F>(
    inputs: &[&Path],
    output: &Path,
    format: Format,
        level: i32,
        on_progress: F,
) -> Result<()>
where
F: Fn(usize, usize, &str),
{
    if inputs.is_empty() {
        bail!("No input paths selected");
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
        .with_context(|| format!("Cannot create output directory: {}", parent.display()))?;
    }

    let files = collect_files(inputs)?;
    if files.is_empty() {
        bail!("No regular files found in the selected input paths");
    }
    ensure_output_not_input(&files, output)?;

    // Write to a sibling temporary file, then rename only after successful completion.
    let file_name = output.file_name().context("Output path has no file name")?;
    let temp = output.with_file_name(format!(
        ".{}.{}.partial",
        file_name.to_string_lossy(),
                                             std::process::id()
    ));
    let _ = fs::remove_file(&temp);

    let result = match format {
        Format::Zip => create_zip(&files, &temp, level, &on_progress),
        Format::Tar | Format::TarGz | Format::TarXz | Format::TarZstd => {
            create_tar(&files, &temp, format, level, &on_progress)
        }
        Format::SevenZ => create_7z(&files, &temp, &on_progress),
        Format::Zstd => create_zstd(&files, &temp, level, &on_progress),
        Format::Xz => create_xz(&files, &temp, level, &on_progress),
    };

    if let Err(err) = result {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }

    if output.exists() {
        fs::remove_file(output)
        .with_context(|| format!("Cannot replace existing output: {}", output.display()))?;
    }
    fs::rename(&temp, output)
    .with_context(|| format!("Cannot finalize archive: {}", output.display()))?;
    Ok(())
}

pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let format = Format::from_path(archive).context("Unsupported archive extension")?;
    fs::create_dir_all(dest)?;
    let dest = dest.canonicalize().context("Cannot resolve extraction directory")?;

    match format {
        Format::Zip => extract_zip(archive, &dest),
        Format::Tar | Format::TarGz | Format::TarXz | Format::TarZstd => {
            extract_tar(archive, &dest, format)
        }
        Format::SevenZ => extract_7z(archive, &dest),
        Format::Xz | Format::Zstd => bail!("Standalone .xz/.zst streams are not archive containers and cannot be extracted as a file list"),
    }
}

pub fn list_archive(path: &Path) -> Result<Vec<Entry>> {
    let format = Format::from_path(path).context("Unsupported archive extension")?;
    match format {
        Format::Zip => list_zip(path),
        Format::Tar | Format::TarGz | Format::TarXz | Format::TarZstd => list_tar(path, format),
        Format::SevenZ => list_7z(path),
        Format::Xz | Format::Zstd => bail!("Standalone .xz/.zst streams do not contain a file index"),
    }
}

fn create_zip<F>(files: &[(PathBuf, PathBuf)], output: &Path, level: i32, progress: &F) -> Result<()>
where F: Fn(usize, usize, &str) {
    let options = FileOptions::default()
    .compression_method(CompressionMethod::Deflated)
    .compression_level(Some(compression_level(Format::Zip, level) as i32))
    .large_file(true);
    let out = File::create(output)?;
    let mut zip = ZipWriter::new(BufWriter::with_capacity(BUFFER_CAPACITY, out));

    for (idx, (full, rel)) in files.iter().enumerate() {
        let name = archive_name(rel)?;
        progress(idx + 1, files.len(), &name);
        zip.start_file(&name, options)?;
        let mut input = BufReader::with_capacity(BUFFER_CAPACITY, File::open(full)?);
        std::io::copy(&mut input, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let mut zip = ZipArchive::new(BufReader::with_capacity(BUFFER_CAPACITY, File::open(archive)?))?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let rel = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => bail!("Unsafe path in ZIP entry: {}", entry.name()),
        };
        validate_relative_path(&rel)?;
        let outpath = dest.join(&rel);
        ensure_no_symlink_ancestors(dest, &outpath)?;

        if entry.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
                ensure_no_symlink_ancestors(dest, &outpath)?;
            }
            let mut out = BufWriter::with_capacity(BUFFER_CAPACITY, File::create(&outpath)?);
            std::io::copy(&mut entry, &mut out)?;
            out.flush()?;
        }
    }
    Ok(())
}

fn list_zip(path: &Path) -> Result<Vec<Entry>> {
    let mut zip = ZipArchive::new(BufReader::with_capacity(BUFFER_CAPACITY, File::open(path)?))?;
    let mut entries = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        entries.push(Entry {
            path: f.name().to_owned(),
                     size: f.size(),
                     compressed: Some(f.compressed_size()),
                     is_dir: f.is_dir(),
        });
    }
    Ok(entries)
}

fn create_tar<F>(files: &[(PathBuf, PathBuf)], output: &Path, format: Format, level: i32, progress: &F) -> Result<()>
where F: Fn(usize, usize, &str) {
    let out = File::create(output)?;
    let buffered = BufWriter::with_capacity(BUFFER_CAPACITY, out);
    let level = compression_level(format, level);

    let writer: Box<dyn Write> = match format {
        Format::Tar => Box::new(buffered),
        Format::TarGz => Box::new(flate2::write::GzEncoder::new(
            buffered, flate2::Compression::new(level),
        )),
        Format::TarXz => Box::new(xz2::write::XzEncoder::new(buffered, level)),
        Format::TarZstd => {
            let encoder = zstd::stream::write::Encoder::new(buffered, level as i32)?;
            // Multithreaded zstd is optional; enable with features = ["zstdmt"] in Cargo.toml.
            Box::new(encoder.auto_finish())
        }
        _ => bail!("Invalid format for TAR writer"),
    };

    let mut tar = tar::Builder::new(writer);
    for (idx, (full, rel)) in files.iter().enumerate() {
        let name = archive_name(rel)?;
        progress(idx + 1, files.len(), &name);
        tar.append_path_with_name(full, rel)
        .with_context(|| format!("Failed to add {name} to archive"))?;
    }
    let mut writer = tar.into_inner()?;
    writer.flush()?;
    Ok(())
}

fn extract_tar(archive_path: &Path, dest: &Path, format: Format) -> Result<()> {
    let file = File::open(archive_path)?;
    let reader: Box<dyn Read> = match format {
        Format::Tar => Box::new(BufReader::with_capacity(BUFFER_CAPACITY, file)),
        Format::TarGz => Box::new(flate2::read::GzDecoder::new(
            BufReader::with_capacity(BUFFER_CAPACITY, file),
        )),
        Format::TarZstd => Box::new(zstd::stream::read::Decoder::new(
            BufReader::with_capacity(BUFFER_CAPACITY, file),
        )?),
        Format::TarXz => Box::new(xz2::read::XzDecoder::new(
            BufReader::with_capacity(BUFFER_CAPACITY, file),
        )),
        _ => bail!("Invalid format for TAR extraction"),
    };

    let mut tar = tar::Archive::new(reader);

    // Deliberately extract only regular files and directories. This avoids
    // creating archive-supplied symlinks, hard links, devices, or other special files.
    for item in tar.entries()? {
        let mut entry = item?;
        let entry_type = entry.header().entry_type();
        let rel = entry
        .path()
        .context("Invalid path in TAR entry")?
        .into_owned();

        validate_relative_path(&rel)
        .with_context(|| format!("Unsafe path in TAR archive: {}", rel.display()))?;

        let target = dest.join(&rel);
        ensure_no_symlink_ancestors(dest, &target)?;

        if entry_type.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }

        if !entry_type.is_file() {
            // Skip links and special entries instead of creating them.
            continue;
        }

        let parent = target
        .parent()
        .context("TAR entry has no parent directory")?;
        fs::create_dir_all(parent)?;
        ensure_no_symlink_ancestors(dest, &target)?;

        // create_new prevents an archive entry from silently overwriting an
        // existing file. Remove this restriction only if overwrite behavior is desired.
        let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .with_context(|| format!("Cannot create extracted file: {}", target.display()))?;
        let mut output = BufWriter::with_capacity(BUFFER_CAPACITY, file);
        std::io::copy(&mut entry, &mut output)?;
        output.flush()?;
    }

    Ok(())
}

fn list_tar(path: &Path, format: Format) -> Result<Vec<Entry>> {
    let file = File::open(path)?;
    let reader: Box<dyn Read> = match format {
        Format::Tar => Box::new(BufReader::with_capacity(BUFFER_CAPACITY, file)),
        Format::TarGz => Box::new(flate2::read::GzDecoder::new(
            BufReader::with_capacity(BUFFER_CAPACITY, file),
        )),
        Format::TarZstd => Box::new(zstd::stream::read::Decoder::new(
            BufReader::with_capacity(BUFFER_CAPACITY, file),
        )?),
        Format::TarXz => Box::new(xz2::read::XzDecoder::new(
            BufReader::with_capacity(BUFFER_CAPACITY, file),
        )),
        _ => bail!("Invalid format for TAR listing"),
    };

    let mut archive = tar::Archive::new(reader);
    let mut entries = Vec::new();
    for item in archive.entries()? {
        let entry = item?;
        entries.push(Entry {
            path: entry.path()?.to_string_lossy().into_owned(),
                     size: entry.header().size()?,
                     compressed: None,
                     is_dir: entry.header().entry_type().is_dir(),
        });
    }
    Ok(entries)
}

fn create_7z<F>(files: &[(PathBuf, PathBuf)], output: &Path, progress: &F) -> Result<()>
where F: Fn(usize, usize, &str) {
    let mut writer = sevenz_rust::SevenZWriter::create(output)?;
    for (idx, (full, rel)) in files.iter().enumerate() {
        let name = archive_name(rel)?;
        progress(idx + 1, files.len(), &name);
        let entry = sevenz_rust::SevenZArchiveEntry::from_path(full, name.clone());
        let input = File::open(full)?;
        writer.push_archive_entry(entry, Some(input))
        .with_context(|| format!("Failed to add {name} to 7z archive"))?;
    }
    writer.finish().map_err(|e| anyhow!("Failed to finish 7z archive: {e}"))?;
    Ok(())
}

fn extract_7z(archive: &Path, dest: &Path) -> Result<()> {
    // The library handles the 7z format. For untrusted archives, extract to a new empty directory.
    sevenz_rust::decompress_file(archive, dest)
    .map_err(|e| anyhow!("7z extraction failed: {e}"))
}

fn list_7z(path: &Path) -> Result<Vec<Entry>> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let archive = sevenz_rust::Archive::read(
        &mut BufReader::new(file),
                                             len,
                                             sevenz_rust::Password::empty().as_slice(),
    ).map_err(|e| anyhow!("Cannot read 7z directory: {e}"))?;

    let mut entries = Vec::with_capacity(archive.files.len());
    for entry in &archive.files {
        entries.push(Entry {
            path: entry.name().to_owned(),
                     size: entry.size(),
                     compressed: None,
                     is_dir: entry.is_directory(),
        });
    }
    Ok(entries)
}

fn create_zstd<F>(files: &[(PathBuf, PathBuf)], output: &Path, level: i32, progress: &F) -> Result<()>
where F: Fn(usize, usize, &str) {
    if files.len() != 1 {
        bail!("Standalone .zst compression accepts exactly one input file");
    }
    let (input_path, rel) = &files[0];
    let name = archive_name(rel)?;
    progress(1, 1, &name);
    let input = BufReader::with_capacity(BUFFER_CAPACITY, File::open(input_path)?);
    let output = BufWriter::with_capacity(BUFFER_CAPACITY, File::create(output)?);
    let mut encoder = zstd::stream::write::Encoder::new(output, compression_level(Format::Zstd, level) as i32)?;
    std::io::copy(&mut input.take(u64::MAX), &mut encoder)?;
    encoder.finish()?;
    Ok(())
}

fn create_xz<F>(files: &[(PathBuf, PathBuf)], output: &Path, level: i32, progress: &F) -> Result<()>
where F: Fn(usize, usize, &str) {
    if files.len() != 1 {
        bail!("Standalone .xz compression accepts exactly one input file");
    }
    let (input_path, rel) = &files[0];
    let name = archive_name(rel)?;
    progress(1, 1, &name);
    let input = BufReader::with_capacity(BUFFER_CAPACITY, File::open(input_path)?);
    let output = BufWriter::with_capacity(BUFFER_CAPACITY, File::create(output)?);
    let mut encoder = xz2::write::XzEncoder::new(output, compression_level(Format::Xz, level));
    std::io::copy(&mut input.take(u64::MAX), &mut encoder)?;
    encoder.finish()?;
    Ok(())
}

fn archive_name(path: &Path) -> Result<String> {
    validate_relative_path(path)?;
    let name = path.to_string_lossy().replace('\\', "/");
    if name.is_empty() {
        bail!("Empty archive entry path");
    }
    Ok(name)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
        {
            bail!("Unsafe archive path: {}", path.display());
        }
        Ok(())
}

fn ensure_no_symlink_ancestors(root: &Path, target: &Path) -> Result<()> {
    let rel = target.strip_prefix(root)
    .context("Output path is outside extraction directory")?;
    let mut current = root.to_path_buf();
    for component in rel.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if let Ok(meta) = fs::symlink_metadata(&current) {
                if meta.file_type().is_symlink() {
                    bail!("Refusing to write through symlink: {}", current.display());
                }
            }
        }
    }
    Ok(())
}
