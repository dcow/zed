use collections::HashMap;
use core_foundation::{
    array::{CFArray, CFArrayRef},
    attributed_string::CFMutableAttributedString,
    base::{CFRange, TCFType},
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use core_graphics::{
    base::{CGGlyph, kCGImageAlphaPremultipliedLast},
    color_space::CGColorSpace,
    context::{CGContext, CGTextDrawingMode},
};
use core_text::{
    font::{CTFont, CTFontRef},
    font_collection::{CTFontCollection, CTFontCollectionRef},
    font_descriptor::{
        CTFontDescriptor, CTFontDescriptorRef, kCTFontSlantTrait, kCTFontSymbolicTrait,
        kCTFontWeightTrait, kCTFontWidthTrait,
    },
    line::CTLine,
    string_attributes::kCTFontAttributeName,
};
use gpui::{
    Bounds, DevicePixels, Font, FontFallbacks, FontFeatures, FontId, FontMetrics, FontRun,
    FontStyle, FontWeight, GlyphId, LineLayout, Pixels, PlatformTextSystem, RenderGlyphParams,
    Result, SUBPIXEL_VARIANTS_X, ShapedGlyph, ShapedRun, SharedString, Size, TextRenderingMode,
    point, px, size, swap_rgba_pa_to_bgra,
};
use parking_lot::RwLock;
use smallvec::SmallVec;
use std::{borrow::Cow, char, sync::Arc};

// CoreText symbolic traits
const CT_FONT_BOLD_TRAIT: u32 = 0x00000002;
const CT_FONT_ITALIC_TRAIT: u32 = 0x00000001;

#[allow(non_upper_case_globals)]
const kCGImageAlphaOnly: u32 = 7;

/// iOS text system using CoreText for font shaping and CGContext for rasterization.
///
/// The CoreText API is identical between iOS and macOS. The only meaningful
/// difference is font discovery: iOS has no `/Library/Fonts` directory, so
/// only system fonts and fonts registered via `CTFontManagerRegisterFontData`
/// are available.
pub struct IosTextSystem(RwLock<IosTextSystemState>);

/// A loaded font, identified by its PostScript name and held as a CTFont.
struct LoadedFont {
    ct_font: CTFont,
    postscript_name: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct FontKey {
    family: SharedString,
    features: FontFeatures,
    fallbacks: Option<FontFallbacks>,
}

struct IosTextSystemState {
    /// Fonts loaded into this text system (in-memory and discovered).
    fonts: Vec<LoadedFont>,
    /// Resolved Font → FontId cache.
    font_selections: HashMap<Font, FontId>,
    /// PostScript name → FontId lookup.
    font_ids_by_postscript_name: HashMap<String, FontId>,
    /// Family key → list of FontIds for candidates.
    font_ids_by_family: HashMap<FontKey, SmallVec<[FontId; 4]>>,
    /// Data for in-memory fonts registered via add_fonts.
    registered_font_data: Vec<Arc<Vec<u8>>>,
}

impl IosTextSystem {
    pub fn new() -> Self {
        Self(RwLock::new(IosTextSystemState {
            fonts: Vec::new(),
            font_selections: HashMap::default(),
            font_ids_by_postscript_name: HashMap::default(),
            font_ids_by_family: HashMap::default(),
            registered_font_data: Vec::new(),
        }))
    }
}

impl Default for IosTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformTextSystem for IosTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.0.write().add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        let mut names = Vec::new();

        // Enumerate system fonts via CoreText — same API as macOS.
        let collection = core_text::font_collection::create_for_all_families();
        unsafe extern "C" {
            fn CTFontCollectionCreateMatchingFontDescriptors(
                collection: CTFontCollectionRef,
            ) -> CFArrayRef;
        }
        let descriptors: Option<CFArray<CTFontDescriptor>> = unsafe {
            let array_ref =
                CTFontCollectionCreateMatchingFontDescriptors(collection.as_concrete_TypeRef());
            if array_ref.is_null() {
                None
            } else {
                Some(CFArray::wrap_under_create_rule(array_ref))
            }
        };
        if let Some(descriptors) = descriptors {
            for descriptor in descriptors.into_iter() {
                if let Some(name) = family_name_for_descriptor(&descriptor) {
                    names.push(name);
                }
            }
        }

        // Also include any in-memory fonts.
        let lock = self.0.read();
        for font in &lock.fonts {
            names.push(font.postscript_name.clone());
        }

        names.sort();
        names.dedup();
        names
    }

    fn font_id(&self, font: &Font) -> Result<FontId> {
        {
            let lock = self.0.read();
            if let Some(font_id) = lock.font_selections.get(font) {
                return Ok(*font_id);
            }
        }
        let mut lock = self.0.write();
        lock.resolve_font(font)
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        let lock = self.0.read();
        let ct_font = &lock.fonts[font_id.0].ct_font;
        let units_per_em = ct_font.units_per_em() as f32;
        let ascent = ct_font.ascent() as f32 / units_per_em;
        let descent = -(ct_font.descent() as f32) / units_per_em;
        let line_gap = ct_font.leading() as f32 / units_per_em;
        let cap_height = ct_font.cap_height() as f32 / units_per_em;
        let x_height = ct_font.x_height() as f32 / units_per_em;
        FontMetrics {
            units_per_em: ct_font.units_per_em(),
            ascent,
            descent,
            line_gap,
            underline_position: ct_font.underline_position() as f32 / units_per_em,
            underline_thickness: ct_font.underline_thickness() as f32 / units_per_em,
            cap_height,
            x_height,
            bounding_box: {
                let bb = ct_font.bounding_box();
                Bounds {
                    origin: point(
                        bb.origin.x as f32 / units_per_em,
                        bb.origin.y as f32 / units_per_em,
                    ),
                    size: size(
                        bb.size.width as f32 / units_per_em,
                        bb.size.height as f32 / units_per_em,
                    ),
                }
            },
        }
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        let lock = self.0.read();
        let ct_font = &lock.fonts[font_id.0].ct_font;
        let glyph = glyph_id.0 as CGGlyph;
        let rect = unsafe {
            ct_font.get_bounding_rects_for_glyphs(
                core_text::font::CTFontOrientation::Default,
                &[glyph],
            )
        };
        Ok(Bounds {
            origin: point(rect.origin.x as f32, rect.origin.y as f32),
            size: size(rect.size.width as f32, rect.size.height as f32),
        })
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        let lock = self.0.read();
        let ct_font = &lock.fonts[font_id.0].ct_font;
        let glyph = glyph_id.0 as CGGlyph;
        let advance = unsafe {
            ct_font.get_advances_for_glyphs(
                core_text::font::CTFontOrientation::Default,
                &[glyph],
                std::ptr::null_mut(),
                1,
            )
        };
        Ok(size(advance as f32, 0.0))
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        let lock = self.0.read();
        let ct_font = &lock.fonts[font_id.0].ct_font;
        let mut chars = [ch as u16];
        let mut glyphs = [0u16];
        let has_glyph = unsafe {
            ct_font.get_glyphs_for_characters(chars.as_ptr(), glyphs.as_mut_ptr(), 1)
        };
        if has_glyph && glyphs[0] != 0 {
            Some(GlyphId(glyphs[0] as u32))
        } else {
            None
        }
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.0.read().raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Bounds<DevicePixels>, Vec<u8>)> {
        if raster_bounds.size.width.0 == 0 || raster_bounds.size.height.0 == 0 {
            return Err(anyhow::anyhow!("glyph has empty bounds"));
        }
        let lock = self.0.read();
        lock.rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.0.read().layout_line(text, font_size, runs)
    }

    fn recommended_rendering_mode(&self) -> gpui::RecommendedRenderingMode {
        // On Retina iOS displays, subpixel AA is not used — we get grayscale AA.
        gpui::RecommendedRenderingMode::Alpha
    }
}

impl IosTextSystemState {
    fn add_fonts(&mut self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        for font_data in fonts {
            let data = font_data.into_owned();
            let data_arc = Arc::new(data);
            // Register the font data with CoreText so CTFont lookups can find it.
            unsafe {
                let cf_data = core_foundation::data::CFData::from_buffer(&**data_arc);
                let mut error: core_foundation::base::CFTypeRef = std::ptr::null();
                CTFontManagerRegisterFontData(cf_data.as_concrete_TypeRef(), 0, &mut error);
            }
            // Load the font into our registry.
            let ct_font = unsafe {
                let cf_data = core_foundation::data::CFData::from_buffer(&**data_arc);
                let descriptor = CTFontManagerCreateFontDescriptorFromData(
                    cf_data.as_concrete_TypeRef(),
                );
                if descriptor.is_null() {
                    continue;
                }
                let desc = CTFontDescriptor::wrap_under_create_rule(descriptor);
                core_text::font::new_from_descriptor(&desc, 12.0)
            };
            let postscript_name = ct_font
                .postscript_name()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let font_id = FontId(self.fonts.len());
            self.font_ids_by_postscript_name
                .insert(postscript_name.clone(), font_id);
            self.fonts.push(LoadedFont {
                ct_font,
                postscript_name,
            });
            self.registered_font_data.push(data_arc);
        }
        Ok(())
    }

    fn resolve_font(&mut self, font: &Font) -> Result<FontId> {
        let font_key = FontKey {
            family: font.family.clone(),
            features: font.features.clone(),
            fallbacks: font.fallbacks.clone(),
        };

        let candidates = if let Some(ids) = self.font_ids_by_family.get(&font_key) {
            ids.clone()
        } else {
            let ids = self.load_family(&font.family)?;
            self.font_ids_by_family.insert(font_key.clone(), ids.clone());
            ids
        };

        if candidates.is_empty() {
            return Err(anyhow::anyhow!(
                "no fonts found for family '{}'",
                font.family
            ));
        }

        // Pick the best match by style/weight.
        let best = candidates
            .iter()
            .min_by_key(|&&id| {
                let ct = &self.fonts[id.0].ct_font;
                let symbolic = ct_symbolic_traits(ct);
                let is_bold = symbolic & CT_FONT_BOLD_TRAIT != 0;
                let is_italic = symbolic & CT_FONT_ITALIC_TRAIT != 0;
                let weight_penalty = if is_bold == font.weight >= FontWeight::BOLD {
                    0u32
                } else {
                    100
                };
                let style_penalty = if is_italic == (font.style == FontStyle::Italic) {
                    0u32
                } else {
                    50
                };
                weight_penalty + style_penalty
            })
            .copied()
            .unwrap_or(candidates[0]);

        self.font_selections.insert(font.clone(), best);
        Ok(best)
    }

    fn load_family(&mut self, family: &str) -> Result<SmallVec<[FontId; 4]>> {
        let mut ids = SmallVec::new();

        // First look in already-registered fonts.
        for (i, font) in self.fonts.iter().enumerate() {
            let family_name = ct_family_name(&font.ct_font);
            if family_name.eq_ignore_ascii_case(family) {
                ids.push(FontId(i));
            }
        }
        if !ids.is_empty() {
            return Ok(ids);
        }

        // Ask CoreText for descriptors in this family.
        let cf_family = CFString::new(family);
        let descriptors = core_text::font_collection::new_from_descriptors(&CFArray::from_CFTypes(
            &[core_text::font_descriptor::new_from_attributes(&{
                let mut attrs = std::collections::HashMap::new();
                attrs.insert(
                    CFString::from_static_string("NSFontFamilyAttribute"),
                    cf_family.as_CFType(),
                );
                CFDictionary::from_CFType_pairs(&attrs)
            })],
        ));

        // Simpler approach: use CTFontCreateWithName and let CoreText handle it.
        let cf_family_name = CFString::new(family);
        let ct_font =
            core_text::font::new_from_name(family, 12.0).map_err(|_| {
                anyhow::anyhow!("font family '{}' not found", family)
            })?;

        let postscript_name = ct_font
            .postscript_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| family.to_string());

        let font_id = FontId(self.fonts.len());
        self.font_ids_by_postscript_name
            .insert(postscript_name.clone(), font_id);
        self.fonts.push(LoadedFont {
            ct_font,
            postscript_name,
        });
        ids.push(font_id);

        Ok(ids)
    }

    fn raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let ct_font = self.ct_font_at_size(params.font_id, params.font_size, params.scale_factor)?;
        let glyph = params.glyph_id.0 as CGGlyph;
        let bounds = unsafe {
            ct_font.get_bounding_rects_for_glyphs(
                core_text::font::CTFontOrientation::Default,
                &[glyph],
            )
        };
        let scale = params.scale_factor;
        let left = (bounds.origin.x as f32 * scale).floor() as i32;
        let bottom = (bounds.origin.y as f32 * scale).floor() as i32;
        let right = ((bounds.origin.x + bounds.size.width) as f32 * scale).ceil() as i32;
        let top = ((bounds.origin.y + bounds.size.height) as f32 * scale).ceil() as i32;
        Ok(Bounds {
            origin: gpui::point(DevicePixels(left), DevicePixels(-top)),
            size: gpui::size(
                DevicePixels((right - left).max(0)),
                DevicePixels((top - bottom).max(0)),
            ),
        })
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Bounds<DevicePixels>, Vec<u8>)> {
        let width = raster_bounds.size.width.0 as usize;
        let height = raster_bounds.size.height.0 as usize;
        let ct_font = self.ct_font_at_size(params.font_id, params.font_size, params.scale_factor)?;
        let glyph = params.glyph_id.0 as CGGlyph;
        let is_emoji = params.is_emoji;

        let (bytes_per_pixel, color_space, alpha_info) = if is_emoji {
            (
                4usize,
                CGColorSpace::create_device_rgb(),
                kCGImageAlphaPremultipliedLast,
            )
        } else {
            (
                1usize,
                CGColorSpace::create_device_gray(),
                kCGImageAlphaOnly,
            )
        };

        let stride = width * bytes_per_pixel;
        let mut pixel_data = vec![0u8; stride * height];

        let context = CGContext::create_bitmap_context(
            Some(pixel_data.as_mut_ptr() as *mut _),
            width,
            height,
            8,
            stride,
            &color_space,
            alpha_info,
        );

        // iOS uses a top-left origin; CoreText uses bottom-left.
        // Flip the coordinate system so CoreText draws into the right part of
        // our bitmap.
        context.translate(
            -raster_bounds.origin.x.0 as f64 / params.scale_factor as f64,
            raster_bounds.origin.y.0 as f64 / params.scale_factor as f64
                + raster_bounds.size.height.0 as f64 / params.scale_factor as f64,
        );
        context.scale(params.scale_factor as f64, -(params.scale_factor as f64));

        if is_emoji {
            context.set_text_drawing_mode(CGTextDrawingMode::CGTextFill);
            unsafe {
                ct_font.draw_glyphs(
                    &[glyph],
                    &[core_graphics::geometry::CGPoint { x: 0.0, y: 0.0 }],
                    context.clone(),
                );
            }
        } else {
            context.set_gray_fill_color(1.0, 1.0);
            context.set_text_drawing_mode(CGTextDrawingMode::CGTextFill);
            context.set_allows_antialiasing(true);
            context.set_should_antialias(true);
            unsafe {
                ct_font.draw_glyphs(
                    &[glyph],
                    &[core_graphics::geometry::CGPoint { x: 0.0, y: 0.0 }],
                    context.clone(),
                );
            }
        }

        if is_emoji {
            swap_rgba_pa_to_bgra(&mut pixel_data);
        }

        Ok((raster_bounds, pixel_data))
    }

    fn ct_font_at_size(
        &self,
        font_id: FontId,
        font_size: Pixels,
        scale_factor: f32,
    ) -> Result<CTFont> {
        let base_font = &self.fonts[font_id.0].ct_font;
        Ok(base_font.clone_with_size(font_size.0 as f64))
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        if runs.is_empty() || text.is_empty() {
            return LineLayout {
                font_size,
                ..Default::default()
            };
        }

        let mut attributed = CFMutableAttributedString::new();
        let cf_str = CFString::new(text);
        attributed.replace_str(&cf_str, CFRange::init(0, 0));

        // Apply font attributes for each run.
        let mut byte_offset = 0usize;
        let mut char_offset = 0usize;
        for run in runs {
            if run.len == 0 {
                continue;
            }
            let run_text = &text[byte_offset..];
            let run_chars = run_text
                .char_indices()
                .take(run.len)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            let cf_range = CFRange::init(char_offset as isize, run.len as isize);
            if let Some(font) = self.fonts.get(run.font_id.0) {
                let ct = font.ct_font.clone_with_size(font_size.0 as f64);
                let ct_ref: CTFontRef = ct.as_concrete_TypeRef();
                unsafe {
                    let key: CFString = CFString::wrap_under_get_rule(kCTFontAttributeName);
                    attributed.set_attribute(cf_range, key.as_concrete_TypeRef(), ct_ref as *const _);
                }
            }
            byte_offset += run_chars;
            char_offset += run.len;
        }

        let line = CTLine::new_with_attributed_string(attributed.as_concrete_TypeRef());
        let width = line.get_typographic_bounds().width as f32;

        // Get runs from the shaped line.
        let ct_runs = line.glyph_runs();
        let mut shaped_runs = Vec::new();
        let mut glyph_x = 0.0f32;

        for ct_run in ct_runs.into_iter() {
            let run_count = ct_run.glyph_count() as usize;
            if run_count == 0 {
                continue;
            }
            let glyphs = ct_run.glyphs();
            let positions = ct_run.positions();
            let string_indices = ct_run.string_indices();

            // Determine which FontId this run belongs to.
            // We look up by the run's CTFont attribute.
            let font_id = self
                .font_id_for_ct_run_font(&ct_run)
                .unwrap_or(runs[0].font_id);

            let shaped_glyphs = glyphs
                .iter()
                .enumerate()
                .map(|(i, &glyph)| {
                    let pos = positions.get(i).copied().unwrap_or_default();
                    ShapedGlyph {
                        id: GlyphId(glyph as u32),
                        position: point(px(pos.x as f32), px(pos.y as f32)),
                        index: string_indices
                            .get(i)
                            .copied()
                            .unwrap_or(i) as usize,
                        is_emoji: glyph_is_emoji(glyph),
                    }
                })
                .collect();

            shaped_runs.push(ShapedRun {
                font_id,
                glyphs: shaped_glyphs,
            });
        }

        // Compute ascent/descent from the first used font.
        let (ascent, descent) = if let Some(font) = runs.first().and_then(|r| self.fonts.get(r.font_id.0)) {
            let ct = font.ct_font.clone_with_size(font_size.0 as f64);
            (px(ct.ascent() as f32), px(ct.descent() as f32))
        } else {
            (px(font_size.0 * 0.8), px(font_size.0 * 0.2))
        };

        LineLayout {
            font_size,
            width: px(width),
            ascent,
            descent,
            runs: shaped_runs,
            len: text.len(),
        }
    }

    fn font_id_for_ct_run_font(&self, _run: &core_text::run::CTRun) -> Option<FontId> {
        // For now, return None to use the fallback.
        // TODO: extract the CTFont from the run's attributes and look it up.
        None
    }
}

fn ct_symbolic_traits(font: &CTFont) -> u32 {
    unsafe {
        let traits_dict = font.copy_traits();
        let key = CFString::from_static_string("NSCTFontSymbolicTrait");
        if let Some(val) = traits_dict.find(key.as_concrete_TypeRef()) {
            let num: CFNumber = CFNumber::wrap_under_get_rule(*val as *const _);
            num.to_i32().unwrap_or(0) as u32
        } else {
            0
        }
    }
}

fn ct_family_name(font: &CTFont) -> String {
    font.family_name().to_string()
}

fn family_name_for_descriptor(descriptor: &CTFontDescriptor) -> Option<String> {
    descriptor
        .family_name()
        .map(|s| s.to_string())
}

fn glyph_is_emoji(glyph: u16) -> bool {
    // CTFont doesn't directly expose whether a glyph is emoji, so we use a
    // conservative heuristic: glyphs in the AppleColorEmoji font are emoji.
    // For now, return false — the renderer will use monochrome mode for all glyphs.
    // TODO: check the font family name of the run.
    false
}

// CoreText private/public CFData-based font registration APIs.
unsafe extern "C" {
    fn CTFontManagerRegisterFontData(
        data: core_foundation::data::CFDataRef,
        scope: u32, // kCTFontManagerScopeProcess = 0
        error: *mut core_foundation::base::CFTypeRef,
    ) -> bool;

    fn CTFontManagerCreateFontDescriptorFromData(
        data: core_foundation::data::CFDataRef,
    ) -> CTFontDescriptorRef;
}

// Missing from core-graphics bindings on iOS.
use core_foundation::dictionary::{CFDictionary, CFMutableDictionary};
