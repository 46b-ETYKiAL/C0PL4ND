//! Content-keyed glyph (galley) + image-texture caches for the grid painter.
//!
//! `GalleyCache` memoises single-glyph egui galleys (keyed by char + colour +
//! pass + style) so the per-cell grid paint never re-shapes a glyph it drew last
//! frame; `ImageTextureCache` memoises uploaded inline-image textures. Both prune
//! what they did not touch each frame. Extracted from the `egui_app` god-module;
//! re-exported via `pub(crate) use glyph_cache::*`. Behaviour unchanged.

use std::collections::{HashMap, HashSet};

use eframe::egui;

use super::grid::PaneId;

/// Which of a grid row's up-to-three painted galleys this cache entry holds: the
/// crisp main pass in the runs' real colours, or one of the two pure-channel
/// chromatic-aberration ghost passes. Each pass for a row is keyed separately so
/// the ghost galleys (drawn in a single override colour) never collide with the
/// crisp galley (drawn in the runs' real per-cell colours).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RowPass {
    /// The crisp main pass — each run keeps its real SGR colour.
    Main,
    /// The pure-red ghost (chromatic aberration), shifted left.
    GhostRed,
    /// The pure-blue ghost (chromatic aberration), shifted right.
    GhostBlue,
}

/// Content-keyed single-glyph galley cache for [`paint_grid_native`]. The grid
/// is painted one glyph per CELL, each positioned at its computed `col * cw`
/// origin — so layout is FONT-ADVANCE-INDEPENDENT: a wide or fallback glyph can
/// never shift another cell, and there is no "scattered text under a proportional
/// font" failure mode (the reason the per-run approach was reverted). The cache
/// is keyed purely by glyph CONTENT (char + colour + pass + style), so the same
/// glyph reused across many cells shares ONE laid-out galley (the galley is
/// position-independent — painted at each cell's origin). Pruned to the glyphs
/// seen this frame so the map stays bounded by distinct on-screen glyph+colour
/// combinations (small). Cleared wholesale on a font re-install (the cached
/// galleys reference the old font atlas).
#[derive(Default)]
pub(crate) struct GalleyCache {
    /// content key -> laid-out single-glyph galley.
    glyphs: HashMap<u64, std::sync::Arc<egui::Galley>>,
    /// Content keys drawn THIS frame, for the end-of-frame prune.
    seen_this_frame: HashSet<u64>,
}

impl GalleyCache {
    /// Lay out a single glyph (content `key`), reusing the cached galley when the
    /// glyph+colour+style is unchanged. `build` constructs the
    /// [`egui::text::LayoutJob`] on a miss. Records the key as seen this frame.
    pub(crate) fn glyph(
        &mut self,
        painter: &egui::Painter,
        key: u64,
        build: impl FnOnce() -> egui::text::LayoutJob,
    ) -> std::sync::Arc<egui::Galley> {
        self.seen_this_frame.insert(key);
        if let Some(galley) = self.glyphs.get(&key) {
            return galley.clone();
        }
        let galley = painter.layout_job(build());
        self.glyphs.insert(key, galley.clone());
        galley
    }

    /// Drop glyph galleys NOT drawn this frame (glyphs that scrolled off / a
    /// closed pane) and reset the per-frame seen set. Called once at the end of
    /// [`C0pl4ndApp::grid_ui`].
    pub(crate) fn prune_unseen(&mut self) {
        if self.seen_this_frame.is_empty() {
            // Nothing painted this frame (e.g. every pane errored): keep entries
            // so a transient empty frame does not evict the whole cache.
            return;
        }
        self.glyphs.retain(|k, _| self.seen_this_frame.contains(k));
        self.seen_this_frame.clear();
    }

    /// Drop every entry — used when the font stack is re-installed (the cached
    /// galleys reference the previous font atlas and must be relaid).
    pub(crate) fn clear(&mut self) {
        self.glyphs.clear();
        self.seen_this_frame.clear();
    }
}

/// Content cache key for one painted glyph: the char, its RGB colour, the pass
/// (crisp / chromatic-ghost), and the shared per-frame style bits (font size +
/// fallback fg). Two cells with the same glyph+colour+style share one galley.
///
/// INVARIANT: the key must capture EVERY input that changes the laid-out galley
/// produced by [`build_glyph_job`]. That function renders `char` + `font` (size
/// via `style_key`, and the BOLD face via a distinct `FontId` family) + `color` +
/// the italic bit — so `attrs` below carries exactly the rendition bits that
/// change the galley. Without them, a bold `a` and a regular `a` in the same
/// colour would collide on one cached galley and the bold one would draw thin.
///
/// Line decorations (underline / strikeout) are deliberately NOT part of the key:
/// they are drawn analytically per-SPAN by the renderer, not baked into the
/// per-glyph galley, so they cannot change it.
pub(crate) fn glyph_cache_key(
    c: char,
    rgb: (u8, u8, u8),
    pass: RowPass,
    style_key: u64,
    attrs: GlyphAttrs,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    style_key.hash(&mut h);
    c.hash(&mut h);
    rgb.hash(&mut h);
    (pass as u8).hash(&mut h);
    attrs.bits().hash(&mut h);
    h.finish()
}

/// The rendition bits that change a single glyph's laid-out galley: whether it
/// is drawn with the bold face and whether it is italic. Kept as its own type so
/// [`glyph_cache_key`] and [`build_glyph_job`] cannot drift apart — adding a bit
/// here forces both to account for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GlyphAttrs {
    /// Draw with the bold font family (and faux-bold when no bold face exists).
    pub(crate) bold: bool,
    /// Draw italic/oblique.
    pub(crate) italic: bool,
}

impl GlyphAttrs {
    /// Pack the bits for hashing — a stable, exhaustive encoding of this type.
    pub(crate) fn bits(self) -> u8 {
        (self.bold as u8) | ((self.italic as u8) << 1)
    }
}

/// Build the [`egui::text::LayoutJob`] for a SINGLE glyph in `color` with the
/// rendition `attrs`. Used on a glyph-cache miss; the resulting galley is painted
/// at the cell's `col * cw` origin.
///
/// `font` already selects the FAMILY (the caller passes the bold family for a
/// bold cell — see `fonts::bold_monospace_family`), so boldness is carried by a
/// real bold face wherever one is installed rather than by a synthetic effect.
/// Italic is applied by egui's own oblique synthesis, which works for every
/// monospace face including those shipping no italic cut.
pub(crate) fn build_glyph_job(
    c: char,
    font: &egui::FontId,
    color: egui::Color32,
    attrs: GlyphAttrs,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let mut buf = [0u8; 4];
    job.append(
        c.encode_utf8(&mut buf),
        0.0,
        egui::text::TextFormat {
            font_id: font.clone(),
            color,
            italics: attrs.italic,
            ..Default::default()
        },
    );
    job
}

/// Texture-cache key for an inline image: `(pane, abs line, col, width, height)`.
/// Stable across a scrollback-view scroll (so a visible image uploads once), and
/// distinct per pane so two panes' images never collide.
pub(crate) type ImageKey = (PaneId, usize, usize, usize, usize);

/// GPU-texture cache for inline images (Sixel / Kitty graphics), mirroring
/// [`GalleyCache`]'s seen-set + end-of-frame prune so textures for images that
/// scrolled off (or a closed pane) are dropped and GPU memory cannot grow
/// without bound. An `egui::TextureHandle` frees its GPU texture on drop, so
/// pruning an entry releases the texture.
#[derive(Default)]
pub(crate) struct ImageTextureCache {
    map: HashMap<ImageKey, egui::TextureHandle>,
    seen_this_frame: HashSet<ImageKey>,
}

impl ImageTextureCache {
    /// Return the texture id for `key`, marking it seen this frame. On a cache
    /// HIT the cached id is returned and `fetch` is NOT called. On a MISS `fetch`
    /// is invoked to produce `(width, height, rgba)`, the texture is uploaded
    /// once, and its id returned — so pixel bytes are copied at most once per
    /// uploaded texture (never on a hit). `None` when `fetch` yields nothing
    /// (e.g. the image was evicted between the metadata sweep and the fetch).
    pub(crate) fn get_or_upload(
        &mut self,
        ctx: &egui::Context,
        key: ImageKey,
        fetch: impl FnOnce() -> Option<(usize, usize, Vec<u8>)>,
    ) -> Option<egui::TextureId> {
        self.seen_this_frame.insert(key);
        if let Some(tex) = self.map.get(&key) {
            return Some(tex.id());
        }
        let (w, h, rgba) = fetch()?;
        // NEAREST keeps pixel art (sixel) crisp; the source is already RGBA.
        let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
        let tex = ctx.load_texture(
            format!("c0pl4nd-img-{}-{}-{}", key.0 .0, key.1, key.2),
            image,
            egui::TextureOptions::NEAREST,
        );
        let id = tex.id();
        self.map.insert(key, tex);
        Some(id)
    }

    /// Drop textures not touched this frame (images scrolled off / closed pane)
    /// and reset the seen set. Called once at the end of `grid_ui`.
    pub(crate) fn prune_unseen(&mut self) {
        self.map.retain(|k, _| self.seen_this_frame.contains(k));
        self.seen_this_frame.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const STYLE: u64 = 7;
    const WHITE: (u8, u8, u8) = (255, 255, 255);

    /// Run `body` inside one real egui frame, so the font atlas the galley layout
    /// needs is available, and hand it a throwaway painter.
    fn with_painter<R>(body: impl FnOnce(&egui::Painter) -> R) -> R {
        let ctx = egui::Context::default();
        // `run_ui` takes an FnMut, so the FnOnce body is parked in an Option and
        // taken on the single pass.
        let mut body = Some(body);
        let mut out = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("glyph-cache-test"),
            ));
            if let Some(body) = body.take() {
                out = Some(body(&painter));
            }
        });
        out.expect("the frame body runs exactly once")
    }

    fn job(c: char) -> egui::text::LayoutJob {
        build_glyph_job(
            c,
            &egui::FontId::monospace(12.0),
            egui::Color32::WHITE,
            GlyphAttrs::default(),
        )
    }

    /// Plain rendition — the attrs every pre-existing key assertion implies.
    const PLAIN: GlyphAttrs = GlyphAttrs {
        bold: false,
        italic: false,
    };

    // ---- glyph_cache_key ----

    /// The cache-key INVARIANT the module documents: the key must capture EVERY
    /// input that changes the laid-out galley. If any two of these collided, two
    /// visually-different cells would share one cached galley and the grid would
    /// paint the wrong glyph/colour.
    #[test]
    fn glyph_cache_key_distinguishes_every_input_that_changes_the_galley() {
        let base = glyph_cache_key('a', WHITE, RowPass::Main, STYLE, PLAIN);
        let variants = [
            (
                "a different char",
                glyph_cache_key('b', WHITE, RowPass::Main, STYLE, PLAIN),
            ),
            (
                "a different colour",
                glyph_cache_key('a', (255, 0, 0), RowPass::Main, STYLE, PLAIN),
            ),
            (
                "the red ghost pass",
                glyph_cache_key('a', WHITE, RowPass::GhostRed, STYLE, PLAIN),
            ),
            (
                "the blue ghost pass",
                glyph_cache_key('a', WHITE, RowPass::GhostBlue, STYLE, PLAIN),
            ),
            (
                "a different style (font size)",
                glyph_cache_key('a', WHITE, RowPass::Main, STYLE + 1, PLAIN),
            ),
        ];
        for (what, key) in variants {
            assert_ne!(base, key, "{what} must not collide with the base glyph");
        }
    }

    /// Bold and italic change the laid-out galley (a different FACE and a
    /// synthesised oblique), so they MUST be part of the key. Before the styled
    /// attributes landed, `build_glyph_job` ignored `CellFlags` entirely and the
    /// key correctly omitted them; now that it honours them, a missing bit here
    /// would serve a regular `a` for a bold `a` — SGR 1 would silently render
    /// thin, which is exactly the defect this pass fixes.
    #[test]
    fn glyph_cache_key_distinguishes_bold_and_italic() {
        let plain = glyph_cache_key('a', WHITE, RowPass::Main, STYLE, PLAIN);
        let bold = glyph_cache_key(
            'a',
            WHITE,
            RowPass::Main,
            STYLE,
            GlyphAttrs {
                bold: true,
                italic: false,
            },
        );
        let italic = glyph_cache_key(
            'a',
            WHITE,
            RowPass::Main,
            STYLE,
            GlyphAttrs {
                bold: false,
                italic: true,
            },
        );
        let both = glyph_cache_key(
            'a',
            WHITE,
            RowPass::Main,
            STYLE,
            GlyphAttrs {
                bold: true,
                italic: true,
            },
        );
        for (what, key) in [("bold", bold), ("italic", italic), ("bold+italic", both)] {
            assert_ne!(plain, key, "{what} must not collide with the plain glyph");
        }
        assert_ne!(bold, italic, "bold and italic are distinct renditions");
        assert_ne!(bold, both, "bold+italic is distinct from bold alone");
        assert_ne!(italic, both, "bold+italic is distinct from italic alone");
    }

    /// The packed bits are an exhaustive, collision-free encoding of the type —
    /// the property `glyph_cache_key` relies on to hash the rendition.
    #[test]
    fn glyph_attrs_bits_are_unique_per_combination() {
        let all = [
            GlyphAttrs {
                bold: false,
                italic: false,
            },
            GlyphAttrs {
                bold: true,
                italic: false,
            },
            GlyphAttrs {
                bold: false,
                italic: true,
            },
            GlyphAttrs {
                bold: true,
                italic: true,
            },
        ];
        let bits: Vec<u8> = all.iter().map(|a| a.bits()).collect();
        let mut sorted = bits.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            bits.len(),
            "every combination packs distinctly"
        );
    }

    /// The italic bit must actually reach the layout job — a key that
    /// distinguishes italic while the job ignores it would cache two entries
    /// that render identically (the inverse failure: a silent no-op attribute).
    #[test]
    fn build_glyph_job_applies_the_italic_bit() {
        let plain = build_glyph_job(
            'a',
            &egui::FontId::monospace(12.0),
            egui::Color32::WHITE,
            GlyphAttrs {
                bold: false,
                italic: false,
            },
        );
        let italic = build_glyph_job(
            'a',
            &egui::FontId::monospace(12.0),
            egui::Color32::WHITE,
            GlyphAttrs {
                bold: false,
                italic: true,
            },
        );
        assert!(!plain.sections[0].format.italics, "plain text is upright");
        assert!(
            italic.sections[0].format.italics,
            "the italic bit must reach TextFormat::italics, not be silently dropped"
        );
    }

    /// The three passes are mutually distinct — a ghost galley (drawn in one
    /// override colour) must never be served for the crisp pass, or vice versa.
    #[test]
    fn glyph_cache_key_keeps_all_three_passes_distinct() {
        let keys = [RowPass::Main, RowPass::GhostRed, RowPass::GhostBlue]
            .map(|p| glyph_cache_key('a', WHITE, p, STYLE, PLAIN));
        assert_ne!(keys[0], keys[1]);
        assert_ne!(keys[1], keys[2]);
        assert_ne!(keys[0], keys[2]);
    }

    /// The same glyph keys identically — the property that makes the cache HIT at
    /// all (the control for the distinctness tests above).
    #[test]
    fn glyph_cache_key_is_stable_for_the_same_glyph() {
        assert_eq!(
            glyph_cache_key('a', WHITE, RowPass::Main, STYLE, PLAIN),
            glyph_cache_key('a', WHITE, RowPass::Main, STYLE, PLAIN),
            "the same glyph must reuse one cached galley"
        );
    }

    // ---- GalleyCache ----

    /// A repeated glyph is laid out ONCE and reused — the whole point of the cache.
    /// `build` invocations are counted, so this observes the real work skipped
    /// rather than asserting on the cache's internals.
    #[test]
    fn galley_cache_lays_a_repeated_glyph_out_only_once() {
        let builds = Cell::new(0);
        with_painter(|painter| {
            let mut cache = GalleyCache::default();
            for _ in 0..3 {
                cache.glyph(painter, 1, || {
                    builds.set(builds.get() + 1);
                    job('a')
                });
            }
        });
        assert_eq!(
            builds.get(),
            1,
            "three paints of one glyph must lay out once"
        );
    }

    /// A DIFFERENT key still lays out — the control proving the cache is keyed, not
    /// just returning the first galley forever.
    #[test]
    fn galley_cache_lays_out_each_distinct_glyph() {
        let builds = Cell::new(0);
        with_painter(|painter| {
            let mut cache = GalleyCache::default();
            for key in [1, 2, 3] {
                cache.glyph(painter, key, || {
                    builds.set(builds.get() + 1);
                    job('a')
                });
            }
        });
        assert_eq!(builds.get(), 3, "each distinct glyph must be laid out");
    }

    /// Glyphs that scrolled off are dropped, glyphs still on screen are kept — so
    /// the map stays bounded by what is actually visible.
    #[test]
    fn prune_unseen_drops_glyphs_that_were_not_painted_this_frame() {
        let builds = Cell::new(0);
        let relaid = with_painter(|painter| {
            let mut cache = GalleyCache::default();
            // Frame 1: paint glyphs 1 and 2.
            cache.glyph(painter, 1, || job('a'));
            cache.glyph(painter, 2, || job('b'));
            cache.prune_unseen();

            // Frame 2: paint only glyph 1 — glyph 2 has scrolled off.
            cache.glyph(painter, 1, || job('a'));
            cache.prune_unseen();

            // Frame 3: glyph 1 must still HIT; glyph 2 must have been evicted.
            cache.glyph(painter, 1, || {
                builds.set(builds.get() + 1);
                job('a')
            });
            let mut two_relaid = false;
            cache.glyph(painter, 2, || {
                two_relaid = true;
                job('b')
            });
            two_relaid
        });
        assert_eq!(
            builds.get(),
            0,
            "a glyph painted every frame must survive the prune"
        );
        assert!(relaid, "a glyph that scrolled off must be evicted");
    }

    /// The documented transient-empty-frame guard: a frame that painted NOTHING
    /// (e.g. every pane errored) must not evict the whole cache, or the next real
    /// frame re-lays out every glyph on screen.
    #[test]
    fn prune_unseen_keeps_the_cache_when_nothing_was_painted() {
        let relaid = Cell::new(false);
        with_painter(|painter| {
            let mut cache = GalleyCache::default();
            cache.glyph(painter, 1, || job('a'));
            cache.prune_unseen();

            // A frame that painted nothing at all.
            cache.prune_unseen();

            cache.glyph(painter, 1, || {
                relaid.set(true);
                job('a')
            });
        });
        assert!(
            !relaid.get(),
            "an empty frame must not evict the cache (transient-frame guard)"
        );
    }

    /// A font re-install must drop every galley — the cached ones reference the old
    /// atlas and would paint from a stale texture.
    #[test]
    fn clear_drops_every_cached_galley() {
        let relaid = Cell::new(false);
        with_painter(|painter| {
            let mut cache = GalleyCache::default();
            cache.glyph(painter, 1, || job('a'));
            cache.clear();
            cache.glyph(painter, 1, || {
                relaid.set(true);
                job('a')
            });
        });
        assert!(relaid.get(), "clear() must force a re-layout");
    }

    // ---- build_glyph_job ----

    /// The job carries the glyph in the requested colour and font, and never wraps
    /// (each glyph is painted at its own cell origin).
    #[test]
    fn build_glyph_job_renders_one_glyph_in_the_requested_colour() {
        let font = egui::FontId::monospace(13.0);
        let j = build_glyph_job('Z', &font, egui::Color32::RED, GlyphAttrs::default());

        assert_eq!(j.text, "Z", "the job must carry exactly the one glyph");
        assert_eq!(
            j.wrap.max_width,
            f32::INFINITY,
            "a single glyph must never wrap"
        );
        let section = j.sections.first().expect("one formatted section");
        assert_eq!(section.format.color, egui::Color32::RED);
        assert_eq!(section.format.font_id, font);
    }

    /// A multi-byte char survives the `encode_utf8` buffer (a 4-byte emoji is the
    /// worst case the grid can hand this).
    #[test]
    fn build_glyph_job_handles_a_multi_byte_char() {
        assert_eq!(
            build_glyph_job(
                '🦀',
                &egui::FontId::monospace(12.0),
                egui::Color32::WHITE,
                GlyphAttrs::default(),
            )
            .text,
            "🦀"
        );
    }

    // ---- ImageTextureCache ----

    fn img_key(col: usize) -> ImageKey {
        (PaneId(1), 0, col, 2, 2)
    }

    fn rgba_2x2() -> Option<(usize, usize, Vec<u8>)> {
        Some((2, 2, vec![255u8; 2 * 2 * 4]))
    }

    /// An image uploads ONCE: the second look-up hits the cache and never re-copies
    /// the pixel bytes (the documented "at most once per uploaded texture").
    #[test]
    fn image_cache_uploads_once_then_hits() {
        let ctx = egui::Context::default();
        let mut cache = ImageTextureCache::default();
        let fetches = Cell::new(0);

        let first = cache.get_or_upload(&ctx, img_key(0), || {
            fetches.set(fetches.get() + 1);
            rgba_2x2()
        });
        let second = cache.get_or_upload(&ctx, img_key(0), || {
            fetches.set(fetches.get() + 1);
            rgba_2x2()
        });

        assert!(first.is_some(), "the first look-up must upload a texture");
        assert_eq!(first, second, "a hit must return the same texture id");
        assert_eq!(fetches.get(), 1, "a cache hit must not re-fetch the pixels");
    }

    /// An image evicted between the metadata sweep and the fetch yields `None`
    /// rather than uploading an empty texture.
    #[test]
    fn image_cache_returns_none_when_the_fetch_yields_nothing() {
        let ctx = egui::Context::default();
        let mut cache = ImageTextureCache::default();
        assert_eq!(cache.get_or_upload(&ctx, img_key(0), || None), None);
    }

    /// Textures for images that scrolled off are dropped (freeing GPU memory), while
    /// those still on screen survive.
    #[test]
    fn image_cache_prune_drops_untouched_textures() {
        let ctx = egui::Context::default();
        let mut cache = ImageTextureCache::default();
        cache.get_or_upload(&ctx, img_key(0), rgba_2x2);
        cache.get_or_upload(&ctx, img_key(1), rgba_2x2);
        cache.prune_unseen();

        // Next frame: only image 0 is still on screen.
        cache.get_or_upload(&ctx, img_key(0), rgba_2x2);
        cache.prune_unseen();

        let refetched = Cell::new(false);
        cache.get_or_upload(&ctx, img_key(1), || {
            refetched.set(true);
            rgba_2x2()
        });
        assert!(
            refetched.get(),
            "a texture that scrolled off must be pruned and re-uploaded"
        );

        let kept = Cell::new(false);
        cache.get_or_upload(&ctx, img_key(0), || {
            kept.set(true);
            rgba_2x2()
        });
        assert!(
            !kept.get(),
            "a texture still on screen must survive the prune"
        );
    }
}
