use anyhow::{bail, Context, Result}; //LIB.RS ENGINE
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

// באפר אופטימלי לכונני NVMe (128KB במקום ברירת המחדל של 8KB)
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
        if name.ends_with(".7z") {
            Some(Self::SevenZ)
        } else if name.ends_with(".tar.zst") || name.ends_with(".tar.zstd") {
            Some(Self::TarZstd)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some(Self::TarXz)
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
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::TarZstd => "tar.zst",
            Format::Zstd => "zst",
            Format::SevenZ => "7z",
            Format::TarXz => "tar.xz",
            Format::Xz => "xz",
        }
    }
}

pub struct Entry {
    pub path: String,
    pub size: u64,
    pub compressed: Option<u64>,
    pub is_dir: bool,
}

pub fn format_size(b: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = K * 1024;
    const G: u64 = M * 1024;
    if b >= G {
        format!("{:.2} GB", b as f64 / G as f64)
    } else if b >= M {
        format!("{:.2} MB", b as f64 / M as f64)
    } else if b >= K {
        format!("{:.1} KB", b as f64 / K as f64)
    } else {
        format!("{} B", b)
    }
}

pub fn collect_files(paths: &[&Path]) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut result = Vec::new();
    for path in paths {
        if path.is_file() {
            let name = path.file_name().context("שם קובץ לא תקין")?;
            result.push((path.to_path_buf(), PathBuf::from(name)));
        } else if path.is_dir() {
            let base = path.parent().unwrap_or(Path::new(""));
            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                let full = entry.path();
                if full.is_file() {
                    let rel = full.strip_prefix(base).context("שגיאה בנתיב")?;
                    result.push((full.to_path_buf(), rel.to_path_buf()));
                }
            }
        } else {
            bail!("הנתיב לא קיים: {}", path.display());
        }
    }
    Ok(result)
}

pub fn list_archive(path: &Path) -> Result<Vec<Entry>> {
    let format = Format::from_path(path).context("פורמט לא מזוהה")?;
    match format {
        Format::Zip => list_zip(path),
        Format::Tar | Format::TarGz | Format::TarXz | Format::TarZstd => list_tar(path, format),
        Format::Xz | Format::Zstd => {
            let meta = fs::metadata(path)?;
            Ok(vec![Entry {
                path: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                size: meta.len(),
                compressed: Some(meta.len()),
                is_dir: false,
            }])
        }
        Format::SevenZ => list_7z(path),
    }
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
    match format {
        Format::Zip => create_zip(inputs, output, level, &on_progress),
        Format::Tar => create_tar(inputs, output, None, &on_progress),
        Format::TarGz => create_tar(inputs, output, Some(Comp::Gzip(level)), &on_progress),
        Format::TarXz => create_tar(inputs, output, Some(Comp::Xz(level)), &on_progress),
        Format::TarZstd => create_tar(inputs, output, Some(Comp::Zstd(level)), &on_progress),
        Format::Xz => {
            if inputs.len() != 1 || inputs[0].is_dir() {
                bail!("XZ תומך רק בקובץ בודד");
            }
            compress_xz(inputs[0], output, level)
        }
        Format::Zstd => {
            if inputs.len() != 1 || inputs[0].is_dir() {
                bail!("ZSTD תומך רק בקובץ בודד");
            }
            compress_zstd(inputs[0], output, level)
        }
        Format::SevenZ => create_7z(inputs, output),
    }
}

pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    let format = Format::from_path(archive).context("פורמט לא מזוהה")?;
    match format {
        Format::Zip => extract_zip(archive, dest),
        Format::Tar | Format::TarGz | Format::TarXz | Format::TarZstd => {
            extract_tar(archive, dest, format)
        }
        Format::Zstd => {
            let name = archive
                .file_stem()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| "output".into());
            decompress_zstd(archive, &dest.join(name))
        }
        Format::SevenZ => extract_7z(archive, dest),
        Format::Xz => {
            let name = archive
                .file_stem()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| "output".into());
            decompress_xz(archive, &dest.join(name))
        }
    }
}

// ============================================================
// מנועי קריאה וכתיבה (עם באפרים מותאמים ל-NVMe)
// ============================================================

fn list_zip(path: &Path) -> Result<Vec<Entry>> {
    let mut archive =
        ZipArchive::new(BufReader::with_capacity(BUFFER_CAPACITY, File::open(path)?))?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let f = archive.by_index(i)?;
        let name = f.name().to_string();
        entries.push(Entry {
            path: name.trim_end_matches('/').to_string(),
            size: f.size(),
            compressed: Some(f.compressed_size()),
            is_dir: f.is_dir() || name.ends_with('/'),
        });
    }
    Ok(entries)
}

fn create_zip<F>(inputs: &[&Path], output: &Path, level: i32, on_progress: &F) -> Result<()>
where
    F: Fn(usize, usize, &str),
{
    let level = level.clamp(0, 9);
    let files = collect_files(inputs)?;
    let total = files.len();
    let mut zip = ZipWriter::new(BufWriter::with_capacity(
        BUFFER_CAPACITY,
        File::create(output)?,
    ));
    let options = FileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(level));

    for (idx, (full, rel)) in files.iter().enumerate() {
        let name = rel.to_string_lossy().replace('\\', "/");
        on_progress(idx + 1, total, &name);
        zip.start_file(&name, options)?;
        let mut f = BufReader::with_capacity(BUFFER_CAPACITY, File::open(full)?);
        std::io::copy(&mut f, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let mut zip = ZipArchive::new(BufReader::with_capacity(
        BUFFER_CAPACITY,
        File::open(archive)?,
    ))?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let outpath = match f.enclosed_name() {
            Some(p) => dest.join(p),
            None => continue,
        };
        if f.name().ends_with('/') {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut outfile = BufWriter::with_capacity(BUFFER_CAPACITY, File::create(&outpath)?);
            std::io::copy(&mut f, &mut outfile)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = f.unix_mode() {
                let _ = fs::set_permissions(&outpath, fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

enum Comp {
    Gzip(i32),
    Zstd(i32),
    Xz(i32),
}

fn list_tar(path: &Path, format: Format) -> Result<Vec<Entry>> {
    let file = File::open(path)?;
    let reader: Box<dyn Read> = match format {
        Format::Tar => Box::new(BufReader::with_capacity(BUFFER_CAPACITY, file)),
        Format::TarGz => Box::new(flate2::read::GzDecoder::new(BufReader::with_capacity(
            BUFFER_CAPACITY,
            file,
        ))),
        Format::TarZstd => Box::new(zstd::stream::read::Decoder::new(BufReader::with_capacity(
            BUFFER_CAPACITY,
            file,
        ))?),
        Format::TarXz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(
            BUFFER_CAPACITY,
            file,
        ))),
        _ => bail!("לא פורמט TAR"),
    };
    let mut archive = tar::Archive::new(reader);
    let mut entries = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let p = entry.path()?.to_string_lossy().to_string();
        let header = entry.header();
        entries.push(Entry {
            path: p,
            size: header.size()?,
            compressed: None,
            is_dir: header.entry_type().is_dir(),
        });
    }
    Ok(entries)
}

fn create_tar<F>(
    inputs: &[&Path],
    output: &Path,
    compression: Option<Comp>,
    on_progress: &F,
) -> Result<()>
where
    F: Fn(usize, usize, &str),
{
    let files = collect_files(inputs)?;
    let total = files.len();
    let out = File::create(output)?;
    let writer: Box<dyn Write> = match compression {
        None => Box::new(BufWriter::with_capacity(BUFFER_CAPACITY, out)),
        Some(Comp::Gzip(level)) => Box::new(flate2::write::GzEncoder::new(
            BufWriter::with_capacity(BUFFER_CAPACITY, out),
            flate2::Compression::new(level.clamp(0, 9) as u32),
        )),
        Some(Comp::Zstd(level)) => Box::new(
            zstd::stream::write::Encoder::new(
                BufWriter::with_capacity(BUFFER_CAPACITY, out),
                level.clamp(1, 22),
            )?
            .auto_finish(),
        ),
        Some(Comp::Xz(_)) => Box::new(xz2::write::XzEncoder::new(
            BufWriter::with_capacity(BUFFER_CAPACITY, out),
            6,
        )),
    };

    let mut builder = tar::Builder::new(writer);
    for (idx, (full, rel)) in files.iter().enumerate() {
        on_progress(idx + 1, total, &rel.to_string_lossy());
        builder.append_path_with_name(full, rel)?;
    }
    builder.finish()?;
    Ok(())
}

fn extract_tar(archive: &Path, dest: &Path, format: Format) -> Result<()> {
    let file = File::open(archive)?;
    let reader: Box<dyn Read> = match format {
        Format::Tar => Box::new(BufReader::with_capacity(BUFFER_CAPACITY, file)),
        Format::TarGz => Box::new(flate2::read::GzDecoder::new(BufReader::with_capacity(
            BUFFER_CAPACITY,
            file,
        ))),
        Format::TarZstd => Box::new(zstd::stream::read::Decoder::new(BufReader::with_capacity(
            BUFFER_CAPACITY,
            file,
        ))?),
        Format::TarXz => Box::new(xz2::read::XzDecoder::new(BufReader::with_capacity(
            BUFFER_CAPACITY,
            file,
        ))),
        _ => bail!("לא פורמט TAR"),
    };
    let mut archive = tar::Archive::new(reader);
    archive.unpack(dest)?;
    Ok(())
}

fn compress_zstd(input: &Path, output: &Path, level: i32) -> Result<()> {
    let mut reader = BufReader::with_capacity(BUFFER_CAPACITY, File::open(input)?);
    let mut writer = zstd::stream::write::Encoder::new(
        BufWriter::with_capacity(BUFFER_CAPACITY, File::create(output)?),
        level.clamp(1, 22),
    )?;
    std::io::copy(&mut reader, &mut writer)?;
    writer.finish()?;
    Ok(())
}

fn decompress_zstd(input: &Path, output: &Path) -> Result<()> {
    let mut reader = zstd::stream::read::Decoder::new(BufReader::with_capacity(
        BUFFER_CAPACITY,
        File::open(input)?,
    ))?;
    let mut writer = BufWriter::with_capacity(BUFFER_CAPACITY, File::create(output)?);
    std::io::copy(&mut reader, &mut writer)?;
    Ok(())
}

fn list_7z(path: &Path) -> Result<Vec<Entry>> {
    let meta = fs::metadata(path)?;
    Ok(vec![Entry {
        path: format!(
            "[7z] {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
        size: meta.len(),
        compressed: Some(meta.len()),
        is_dir: false,
    }])
}

fn create_7z(inputs: &[&Path], output: &Path) -> Result<()> {
    if inputs.is_empty() {
        bail!("לא נבחרו קבצים");
    }
    let src = inputs[0];
    sevenz_rust::compress_to_path(src, output).map_err(|e| anyhow::anyhow!("שגיאת 7z: {}", e))?;
    Ok(())
}

fn extract_7z(archive: &Path, dest: &Path) -> Result<()> {
    sevenz_rust::decompress_file(archive, dest)
        .map_err(|e| anyhow::anyhow!("שגיאת פריסת 7z: {}", e))?;
    Ok(())
}

fn compress_xz(input: &Path, output: &Path, _level: i32) -> Result<()> {
    let mut reader = BufReader::with_capacity(BUFFER_CAPACITY, File::open(input)?);
    let mut writer = xz2::write::XzEncoder::new(
        BufWriter::with_capacity(BUFFER_CAPACITY, File::create(output)?),
        6,
    );
    std::io::copy(&mut reader, &mut writer)?;
    writer.finish()?;
    Ok(())
}

fn decompress_xz(input: &Path, output: &Path) -> Result<()> {
    let mut reader = xz2::read::XzDecoder::new(BufReader::with_capacity(
        BUFFER_CAPACITY,
        File::open(input)?,
    ));
    let mut writer = BufWriter::with_capacity(BUFFER_CAPACITY, File::create(output)?);
    std::io::copy(&mut reader, &mut writer)?;
    Ok(())
}
