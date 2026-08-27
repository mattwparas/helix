//! Rendering of non-text documents (images, PDFs) inside the terminal.
//!
//! Media documents hold no text; instead they carry a [`MediaState`] pointing
//! at a rasterized PNG for the current page. The editor renders them with the
//! Kitty graphics protocol's *unicode placeholder* placements: the image is
//! transmitted once out-of-band (`a=T,U=1`) and then anchored to ordinary text
//! cells (U+10EEEE + row/column diacritics, image id encoded in the foreground
//! color). Because placements are plain cells, they survive the cell-diffing
//! renderer without any compositor changes.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The character used for unicode placeholder cells.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics from the kitty spec (`gen/rowcolumn-diacritics.txt`,
/// Unicode ccc=230 combining marks). Index n encodes row/column n+1.
#[rustfmt::skip]
pub const ROW_COLUMN_DIACRITICS: [char; 297] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065A}', '\u{065B}', '\u{065D}', '\u{065E}', '\u{06D6}', '\u{06D7}', '\u{06D8}',
    '\u{06D9}', '\u{06DA}', '\u{06DB}', '\u{06DC}', '\u{06DF}', '\u{06E0}', '\u{06E1}', '\u{06E2}',
    '\u{06E4}', '\u{06E7}', '\u{06E8}', '\u{06EB}', '\u{06EC}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073A}', '\u{073D}', '\u{073F}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074A}', '\u{07EB}', '\u{07EC}', '\u{07ED}', '\u{07EE}',
    '\u{07EF}', '\u{07F0}', '\u{07F1}', '\u{07F3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081B}', '\u{081C}', '\u{081D}', '\u{081E}', '\u{081F}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082A}', '\u{082B}', '\u{082C}',
    '\u{082D}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0F82}', '\u{0F83}', '\u{0F86}', '\u{0F87}',
    '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}', '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}',
    '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}', '\u{1B6B}', '\u{1B6D}',
    '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}',
    '\u{1DC5}', '\u{1DC6}', '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}',
    '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}', '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}',
    '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}', '\u{1DE0}', '\u{1DE1}',
    '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}',
    '\u{20E9}', '\u{20F0}', '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}',
    '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}', '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}',
    '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}', '\u{2DF1}', '\u{2DF2}',
    '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}',
    '\u{A6F0}', '\u{A6F1}', '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}',
    '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}', '\u{A8EA}', '\u{A8EB}', '\u{A8EC}', '\u{A8ED}',
    '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}', '\u{AAB3}', '\u{AAB7}',
    '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}', '\u{FE23}',
    '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}', '\u{1D187}',
    '\u{1D188}', '\u{1D189}', '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}', '\u{1D243}',
    '\u{1D244}',
];

/// How image/PDF rendering is enabled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageRenderingConfig {
    /// Detect terminals known to support kitty unicode placeholders.
    #[default]
    Auto,
    /// Force the kitty graphics protocol.
    Kitty,
    /// Never render media documents graphically.
    Disabled,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsMode {
    #[default]
    None,
    Kitty,
}

pub fn detect_graphics_mode(config: ImageRenderingConfig) -> GraphicsMode {
    match config {
        ImageRenderingConfig::Disabled => GraphicsMode::None,
        ImageRenderingConfig::Kitty => GraphicsMode::Kitty,
        ImageRenderingConfig::Auto => {
            let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
            let term_program = std::env::var("TERM_PROGRAM")
                .unwrap_or_default()
                .to_lowercase();
            // Terminals known to implement unicode placeholder placements.
            // (WezTerm implements the protocol but not placeholders, so it is
            // deliberately absent; use `image-rendering = "kitty"` to force.)
            let known = ["kitty", "ghostty"];
            if known
                .iter()
                .any(|t| term.contains(t) || term_program.contains(t))
                || std::env::var_os("KITTY_WINDOW_ID").is_some()
            {
                GraphicsMode::Kitty
            } else {
                GraphicsMode::None
            }
        }
    }
}

/// How many images to leave loaded in the terminal. A rasterized PDF page is
/// tens of megabytes decoded, and terminals cap their image storage (kitty and
/// ghostty at 320MB per window) by evicting images themselves — which blanks
/// out placements that are still on screen. Paging through a document would
/// otherwise hand the terminal a new image per page, so we free the ones we
/// have paged away from and let them be retransmitted from the PNG cache.
const MAX_LOADED_IMAGES: usize = 4;

#[derive(Debug, Clone, Copy)]
struct Placement {
    /// (cols, rows) the virtual placement was transmitted with.
    size: (u16, u16),
    /// Frame this image was last drawn in.
    used: u64,
}

/// Per-editor terminal graphics state: what has been transmitted, and what is
/// queued for transmission on the next flush.
#[derive(Debug, Default)]
pub struct GraphicsState {
    pub mode: GraphicsMode,
    /// Window size in pixels, if the terminal reports it. Refreshed per frame.
    pub window_px: Option<(u16, u16)>,
    /// Escape sequences to write to the terminal before the next draw.
    pending: Vec<String>,
    /// image id -> the virtual placement already transmitted for it.
    placements: HashMap<u32, Placement>,
    /// Incremented per rendered frame, to tell images drawn in this frame from
    /// ones left over from earlier pages.
    frame: u64,
}

impl GraphicsState {
    /// Queue a (re)transmission if this raster has no placement yet or the
    /// placement size changed. Returns false when graphics are unavailable.
    pub fn ensure_placement(&mut self, raster: &Raster, cols: u16, rows: u16) -> bool {
        match self.mode {
            GraphicsMode::None => false,
            GraphicsMode::Kitty => {
                let frame = self.frame;
                match self.placements.get_mut(&raster.id) {
                    Some(placement) if placement.size == (cols, rows) => placement.used = frame,
                    slot => {
                        if slot.is_some() {
                            self.placements.remove(&raster.id);
                        }
                        self.pending.push(transmit_escape(raster, cols, rows));
                        self.placements.insert(
                            raster.id,
                            Placement {
                                size: (cols, rows),
                                used: frame,
                            },
                        );
                        self.evict_unused();
                    }
                }
                true
            }
        }
    }

    /// Release images that were not drawn in the current frame, oldest first,
    /// until at most [`MAX_LOADED_IMAGES`] remain. Images drawn this frame are
    /// never evicted, so several media splits can stay on screen at once.
    fn evict_unused(&mut self) {
        if self.placements.len() <= MAX_LOADED_IMAGES {
            return;
        }
        let frame = self.frame;
        let mut stale: Vec<(u64, u32)> = self
            .placements
            .iter()
            .filter(|(_, placement)| placement.used < frame)
            .map(|(id, placement)| (placement.used, *id))
            .collect();
        stale.sort_unstable();
        let excess = self.placements.len() - MAX_LOADED_IMAGES;
        for (_, id) in stale.into_iter().take(excess) {
            self.placements.remove(&id);
            self.pending.push(delete_escape(id));
        }
    }

    pub fn take_pending(&mut self) -> Vec<String> {
        self.frame = self.frame.wrapping_add(1);
        std::mem::take(&mut self.pending)
    }

    /// Forget everything transmitted (e.g. after the terminal was released and
    /// reclaimed on suspend/resume); placements will be retransmitted lazily.
    pub fn reset(&mut self) {
        self.placements.clear();
        self.pending.clear();
    }

    /// Cell size in pixels, if the terminal reports window pixel dimensions.
    pub fn cell_px(&self, total_cols: u16, total_rows: u16) -> Option<(f32, f32)> {
        let (w, h) = self.window_px?;
        if total_cols == 0 || total_rows == 0 || w == 0 || h == 0 {
            return None;
        }
        Some((w as f32 / total_cols as f32, h as f32 / total_rows as f32))
    }
}

/// Compute a placement size (cols, rows) that fits `img_px` inside `avail`
/// cells preserving aspect ratio. Unless `allow_upscale` is set (used for
/// PDFs, whose rasters can always be re-rendered larger), an image is never
/// upscaled past its natural size when real pixel metrics are known.
pub fn fit_placement(
    img_px: (u32, u32),
    avail: (u16, u16),
    cell_px: Option<(f32, f32)>,
    allow_upscale: bool,
) -> (u16, u16) {
    let (iw, ih) = (img_px.0.max(1) as f32, img_px.1.max(1) as f32);
    let ((cw, ch), cap_natural) = match cell_px {
        Some(px) => (px, !allow_upscale),
        None => ((8.0, 16.0), false),
    };
    let max_cells = ROW_COLUMN_DIACRITICS.len() as u16;
    let avail = (avail.0.min(max_cells), avail.1.min(max_cells));
    let mut scale = f32::min(avail.0 as f32 * cw / iw, avail.1 as f32 * ch / ih);
    if cap_natural {
        scale = scale.min(1.0);
    }
    let cols = ((iw * scale / cw).round() as u16).clamp(1, avail.0.max(1));
    let rows = ((ih * scale / ch).round() as u16).clamp(1, avail.1.max(1));
    (cols, rows)
}

/// The symbol for the placeholder cell at (row, col) of a placement,
/// or None if out of the addressable range.
pub fn placeholder_symbol(row: u16, col: u16) -> Option<[char; 3]> {
    let r = *ROW_COLUMN_DIACRITICS.get(row as usize)?;
    let c = *ROW_COLUMN_DIACRITICS.get(col as usize)?;
    Some([PLACEHOLDER, r, c])
}

fn transmit_escape(raster: &Raster, cols: u16, rows: u16) -> String {
    // t=f: payload is a path to a PNG the terminal reads itself, so the
    // escape stays tiny. U=1: virtual placement (unicode placeholders).
    // q=2: never send responses (we do not parse APC replies).
    format!(
        "\x1b_Ga=T,U=1,q=2,f=100,t=f,i={},c={},r={};{}\x1b\\",
        raster.id,
        cols,
        rows,
        base64(raster.png.to_string_lossy().as_bytes()),
    )
}

/// Free an image and all of its placements (`d=I`, uppercase: also frees the
/// image data). The PNG stays in our on-disk cache, so a page we come back to
/// is retransmitted without re-rasterizing.
fn delete_escape(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Pdf,
}

/// Detect whether a path should open as a media document, by extension.
pub fn detect_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "pdf" => Some(MediaKind::Pdf),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "heic"
        | "heif" | "avif" | "svg" => Some(MediaKind::Image),
        _ => None,
    }
}

/// A rasterized page: a PNG on disk plus its pixel dimensions and the kitty
/// image id it is transmitted under.
#[derive(Debug, Clone)]
pub struct Raster {
    pub png: PathBuf,
    pub width: u32,
    pub height: u32,
    pub id: u32,
}

#[derive(Debug)]
pub struct MediaState {
    pub kind: MediaKind,
    source: PathBuf,
    mtime: SystemTime,
    /// Zero-based page for PDFs; always 0 for images. This is the page the
    /// document *should* show: paging only moves this counter, and the page is
    /// rasterized off the main thread afterwards (see
    /// [`MediaState::take_raster_request`]). Holding a paging key therefore
    /// runs `pdftoppm` for the page you land on rather than once per keystroke
    /// for pages that are never displayed.
    pub page: usize,
    pub page_count: Option<usize>,
    /// The page [`MediaState::raster`] holds.
    rastered: usize,
    /// Page a background rasterize is currently running for, if any.
    pending: Option<usize>,
    pub raster: Raster,
}

/// A page to rasterize off the main thread. See
/// [`MediaState::take_raster_request`].
#[derive(Debug, Clone)]
pub struct RasterRequest {
    source: PathBuf,
    mtime: SystemTime,
    page: usize,
}

impl RasterRequest {
    pub fn page(&self) -> usize {
        self.page
    }

    /// Rasterize the page. Blocking: run this on a blocking task.
    pub fn run(&self) -> Result<Raster> {
        raster_pdf_page(&self.source, self.mtime, self.page)
    }
}

impl MediaState {
    pub fn open(kind: MediaKind, path: &Path) -> Result<Self> {
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let (raster, page_count) = match kind {
            MediaKind::Image => (image_to_png(path, mtime)?, None),
            MediaKind::Pdf => (raster_pdf_page(path, mtime, 0)?, pdf_page_count(path)),
        };
        Ok(Self {
            kind,
            source: path.to_path_buf(),
            mtime,
            page: 0,
            page_count,
            rastered: 0,
            pending: None,
            raster,
        })
    }

    /// Switch to a zero-based page. The page is rasterized lazily, by
    /// [`MediaState::ensure_raster`] at render time; this only fails when the
    /// page is known not to exist.
    pub fn goto_page(&mut self, page: usize) -> Result<()> {
        if self.kind != MediaKind::Pdf {
            bail!("not a paginated document");
        }
        if let Some(count) = self.page_count {
            if page >= count {
                bail!("no page {} (document has {})", page + 1, count);
            }
        }
        self.page = page;
        Ok(())
    }

    /// Rasterize the current page if [`MediaState::raster`] is stale, blocking
    /// until it is ready. On failure the document falls back to the page it
    /// still has a raster for, so a bad page never leaves the view blank.
    pub fn ensure_raster(&mut self) -> Result<()> {
        if self.rastered == self.page {
            return Ok(());
        }
        let raster = raster_pdf_page(&self.source, self.mtime, self.page);
        self.finish_raster(self.page, raster)
    }

    /// Whether [`MediaState::raster`] still shows an earlier page than
    /// [`MediaState::page`], i.e. a rasterize is outstanding.
    pub fn is_rastering(&self) -> bool {
        self.rastered != self.page
    }

    /// The page to rasterize in the background, or None when the raster is up
    /// to date or a rasterize is already running. Only one runs at a time, so
    /// holding a paging key never queues a `pdftoppm` per keystroke: pages
    /// that go by before their turn are never rendered at all, and the one you
    /// land on is picked up on the next frame.
    pub fn take_raster_request(&mut self) -> Option<RasterRequest> {
        if self.kind != MediaKind::Pdf || self.rastered == self.page || self.pending.is_some() {
            return None;
        }
        self.pending = Some(self.page);
        Some(RasterRequest {
            source: self.source.clone(),
            mtime: self.mtime,
            page: self.page,
        })
    }

    /// Install the result of a rasterize, or discard it if paging has moved on
    /// since it started.
    pub fn finish_raster(&mut self, page: usize, raster: Result<Raster>) -> Result<()> {
        if self.pending == Some(page) {
            self.pending = None;
        }
        if page != self.page {
            return Ok(());
        }
        match raster {
            Ok(raster) => {
                self.raster = raster;
                self.rastered = page;
                Ok(())
            }
            Err(err) => {
                self.page = self.rastered;
                Err(err)
            }
        }
    }
}

fn cache_slot(source: &Path, mtime: SystemTime, page: usize) -> Result<(PathBuf, u32)> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    mtime.hash(&mut hasher);
    page.hash(&mut hasher);
    let h = hasher.finish();
    let id = ((h as u32) & 0xFF_FFFF).max(1);
    let dir = helix_loader::cache_dir().join("media-render");
    std::fs::create_dir_all(&dir).context("creating media render cache dir")?;
    Ok((dir.join(format!("{h:016x}.png")), id))
}

/// Parse width/height out of a PNG's IHDR chunk.
fn png_dimensions(path: &Path) -> Result<(u32, u32)> {
    use std::io::Read;
    let mut head = [0u8; 24];
    std::fs::File::open(path)?
        .read_exact(&mut head)
        .context("reading PNG header")?;
    if &head[0..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        bail!("not a PNG file");
    }
    let w = u32::from_be_bytes(head[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(head[20..24].try_into().unwrap());
    Ok((w, h))
}

/// Run a conversion command, mapping missing binaries to None so callers can
/// try the next tool.
fn try_tool(cmd: &str, args: &[&std::ffi::OsStr]) -> Option<Result<()>> {
    match Command::new(cmd).args(args).output() {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => Some(Err(err.into())),
        Ok(out) if !out.status.success() => Some(Err(anyhow!(
            "`{cmd}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
        Ok(_) => Some(Ok(())),
    }
}

fn image_to_png(source: &Path, mtime: SystemTime) -> Result<Raster> {
    let is_png = source
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let (cache, id) = cache_slot(source, mtime, 0)?;
    let png = if is_png {
        source.to_path_buf()
    } else {
        if !cache.exists() {
            // `[0]` selects the first frame of animated formats in ImageMagick.
            let magick_src: std::ffi::OsString = {
                let mut s = source.as_os_str().to_os_string();
                s.push("[0]");
                s
            };
            let converted = try_tool("magick", &[magick_src.as_os_str(), cache.as_os_str()])
                .or_else(|| try_tool("convert", &[magick_src.as_os_str(), cache.as_os_str()]))
                .or_else(|| {
                    try_tool(
                        "sips",
                        &[
                            "-s".as_ref(),
                            "format".as_ref(),
                            "png".as_ref(),
                            source.as_os_str(),
                            "--out".as_ref(),
                            cache.as_os_str(),
                        ],
                    )
                });
            match converted {
                Some(Ok(())) => {}
                Some(Err(err)) => return Err(err.context("converting image to PNG")),
                None => bail!("no image converter found (need `magick`, `convert`, or `sips`)"),
            }
        }
        cache
    };
    let (width, height) = png_dimensions(&png)?;
    Ok(Raster {
        png,
        width,
        height,
        id,
    })
}

fn raster_pdf_page(source: &Path, mtime: SystemTime, page: usize) -> Result<Raster> {
    let (cache, id) = cache_slot(source, mtime, page)?;
    if !cache.exists() {
        // pdftoppm appends ".png" itself with -singlefile; pass the stem.
        let prefix = cache.with_extension("");
        let page_arg = (page + 1).to_string();
        let ran = try_tool(
            "pdftoppm",
            &[
                "-png".as_ref(),
                "-r".as_ref(),
                "288".as_ref(),
                "-f".as_ref(),
                page_arg.as_str().as_ref(),
                "-l".as_ref(),
                page_arg.as_str().as_ref(),
                "-singlefile".as_ref(),
                source.as_os_str(),
                prefix.as_os_str(),
            ],
        );
        match ran {
            Some(Ok(())) => {}
            Some(Err(err)) => return Err(err.context("rasterizing PDF page")),
            None => bail!("`pdftoppm` (poppler) is required to render PDFs"),
        }
        if !cache.exists() {
            bail!("no page {} in {}", page + 1, source.display());
        }
    }
    let (width, height) = png_dimensions(&cache)?;
    Ok(Raster {
        png: cache,
        width,
        height,
        id,
    })
}

fn pdf_page_count(source: &Path) -> Option<usize> {
    let out = Command::new("pdfinfo").arg(source).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"/tmp/a.png"), "L3RtcC9hLnBuZw==");
    }

    #[test]
    fn fit_preserves_aspect_and_bounds() {
        // 2:1 image in a square-ish view with 8x16 cells: cols ~ 4x rows.
        let (cols, rows) = fit_placement((1000, 500), (80, 40), Some((8.0, 16.0)), false);
        assert!(cols <= 80 && rows <= 40);
        let aspect = (cols as f32 * 8.0) / (rows as f32 * 16.0);
        assert!((aspect - 2.0).abs() < 0.2, "aspect {aspect}");
        // Never upscaled past natural size when pixel metrics are known.
        let (cols, rows) = fit_placement((80, 32), (100, 50), Some((8.0, 16.0)), false);
        assert_eq!((cols, rows), (10, 2));
        // ... unless upscaling is allowed (PDFs).
        let (cols, rows) = fit_placement((80, 32), (100, 50), Some((8.0, 16.0)), true);
        assert!(cols > 10 && rows > 2);
        // Degenerate inputs stay in range.
        let (cols, rows) = fit_placement((1, 1), (0, 0), None, false);
        assert!(cols >= 1 && rows >= 1);
    }

    fn test_raster(id: u32) -> Raster {
        Raster {
            png: PathBuf::from("/tmp/page.png"),
            width: 1000,
            height: 1000,
            id,
        }
    }

    #[test]
    fn paging_frees_images_it_has_left_behind() {
        let mut state = GraphicsState {
            mode: GraphicsMode::Kitty,
            ..Default::default()
        };
        let pages = MAX_LOADED_IMAGES as u32 + 2;
        let mut escapes = String::new();
        // One new image per frame, as paging through a PDF does.
        for id in 1..=pages {
            assert!(state.ensure_placement(&test_raster(id), 10, 10));
            escapes.extend(state.take_pending());
        }
        assert!(state.placements.len() <= MAX_LOADED_IMAGES);
        // The page on screen is still loaded, the first pages were freed.
        assert!(state.placements.contains_key(&pages));
        assert!(!state.placements.contains_key(&1));
        assert!(escapes.contains("a=d,d=I,i=1,"), "no delete for image 1");
    }

    #[test]
    fn images_drawn_this_frame_are_kept() {
        let mut state = GraphicsState {
            mode: GraphicsMode::Kitty,
            ..Default::default()
        };
        // Many media splits on screen at once: all of them are drawn in the
        // same frame, so none may be evicted even past the limit.
        let ids = 1..=(MAX_LOADED_IMAGES as u32 + 2);
        for id in ids.clone() {
            state.ensure_placement(&test_raster(id), 10, 10);
        }
        assert_eq!(state.placements.len(), ids.count());
        assert!(!state.take_pending().iter().any(|e| e.contains("a=d")));
    }

    fn pdf_state(page_count: Option<usize>) -> MediaState {
        MediaState {
            kind: MediaKind::Pdf,
            source: PathBuf::from("/nonexistent.pdf"),
            mtime: SystemTime::UNIX_EPOCH,
            page: 0,
            page_count,
            rastered: 0,
            pending: None,
            raster: test_raster(1),
        }
    }

    #[test]
    fn paging_rasterizes_only_the_page_landed_on() {
        let mut state = pdf_state(Some(10));
        state.goto_page(1).unwrap();
        let request = state.take_raster_request().expect("page 2 rasterizes");
        assert_eq!(request.page(), 1);
        // Holding `j`: more pages go by while that rasterize is still running.
        for page in 2..=5 {
            state.goto_page(page).unwrap();
        }
        assert!(
            state.take_raster_request().is_none(),
            "only one rasterize runs at a time"
        );
        // Its result is for a page long gone, so it is dropped, not displayed.
        state.finish_raster(1, Ok(test_raster(2))).unwrap();
        assert_eq!(state.raster.id, 1);
        assert!(state.is_rastering());
        // The next frame picks up the page actually landed on.
        let request = state.take_raster_request().expect("page 6 rasterizes");
        assert_eq!(request.page(), 5);
        state.finish_raster(5, Ok(test_raster(2))).unwrap();
        assert_eq!(state.raster.id, 2);
        assert!(!state.is_rastering());
        assert!(state.take_raster_request().is_none());
    }

    #[test]
    fn a_page_that_cannot_be_rendered_falls_back() {
        // Without `pdfinfo` there is no page count to range check against, so
        // paging past the end only fails once the rasterize comes back.
        let mut state = pdf_state(None);
        state.goto_page(4).unwrap();
        let request = state.take_raster_request().unwrap();
        assert!(state
            .finish_raster(request.page(), Err(anyhow!("no page 5")))
            .is_err());
        assert_eq!(state.page, 0, "stays on the page it can still show");
        assert!(!state.is_rastering());

        // With a page count it is rejected up front instead.
        let mut state = pdf_state(Some(3));
        assert!(state.goto_page(3).is_err());
        assert_eq!(state.page, 0);
    }

    #[test]
    fn placeholder_symbols_are_cell_sized() {
        // Symbols must stay within the Cell symbol capacity (28 bytes) and
        // form a single width-1 grapheme.
        let mut s = String::new();
        for &(r, c) in &[(0u16, 0u16), (296, 296)] {
            let chars = placeholder_symbol(r, c).unwrap();
            s.clear();
            s.extend(chars);
            assert!(s.len() <= 28, "symbol too long: {} bytes", s.len());
        }
        assert!(placeholder_symbol(297, 0).is_none());
    }
}
