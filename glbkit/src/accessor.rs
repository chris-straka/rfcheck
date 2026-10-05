// SPDX-License-Identifier: MIT
//! Accessor layout resolution and element IO over the BIN chunk.
//!
//! Callers read the accessor and bufferView fields from their own JSON
//! value into a [`Desc`]; [`Layout::resolve`] validates it against the BIN
//! length once, after which every element read is bounds-checked and
//! returns `None` instead of panicking.

use std::fmt;

/// glTF `componentType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    I8,
    U8,
    I16,
    U16,
    U32,
    F32,
}

impl Component {
    pub fn from_gl(code: u64) -> Option<Component> {
        Some(match code {
            5120 => Component::I8,
            5121 => Component::U8,
            5122 => Component::I16,
            5123 => Component::U16,
            5125 => Component::U32,
            5126 => Component::F32,
            _ => return None,
        })
    }

    pub fn gl(self) -> u32 {
        match self {
            Component::I8 => 5120,
            Component::U8 => 5121,
            Component::I16 => 5122,
            Component::U16 => 5123,
            Component::U32 => 5125,
            Component::F32 => 5126,
        }
    }

    pub fn size(self) -> usize {
        match self {
            Component::I8 | Component::U8 => 1,
            Component::I16 | Component::U16 => 2,
            Component::U32 | Component::F32 => 4,
        }
    }
}

/// Components per element for an accessor `type`. MAT2/MAT3 are left out:
/// their column padding is never produced by the exporters we read.
pub fn type_width(kind: &str) -> Option<usize> {
    Some(match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT4" => 16,
        _ => return None,
    })
}

/// The bufferView fields a layout needs.
#[derive(Debug, Clone, Copy, Default)]
pub struct View {
    pub buffer: usize,
    pub byte_offset: usize,
    pub byte_length: usize,
    /// `None` means tightly packed.
    pub byte_stride: Option<usize>,
}

/// The accessor fields a layout needs (absent JSON fields at their glTF
/// defaults).
#[derive(Debug, Clone, Copy)]
pub struct Desc<'a> {
    pub count: usize,
    pub component_type: u64,
    pub kind: &'a str,
    pub normalized: bool,
    pub byte_offset: usize,
    /// `None`: the accessor has no bufferView (all zeros, per spec).
    pub view: Option<View>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    BadComponent(u64),
    BadType(String),
    /// The view lives in an external buffer (only GLB buffer 0 is read).
    ExternalBuffer(usize),
    StrideTooSmall {
        stride: usize,
        elem: usize,
    },
    Overflow,
    /// The bufferView runs past the BIN chunk.
    ViewPastBin {
        end: usize,
        bin_len: usize,
    },
    /// The accessor's last element runs past its bufferView.
    PastView {
        end: usize,
        view_end: usize,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::BadComponent(c) => write!(f, "componentType {c}"),
            LayoutError::BadType(t) => write!(f, "type {t:?}"),
            LayoutError::ExternalBuffer(b) => write!(f, "external buffer {b}"),
            LayoutError::StrideTooSmall { stride, elem } => {
                write!(f, "byteStride {stride} below element size {elem}")
            }
            LayoutError::Overflow => f.write_str("extent overflows"),
            LayoutError::ViewPastBin { end, bin_len } => {
                write!(f, "bufferView reads past the BIN chunk ({end} > {bin_len})")
            }
            LayoutError::PastView { end, view_end } => {
                write!(f, "reads past its bufferView ({end} > {view_end})")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

/// Where an accessor's elements live in the BIN chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Byte offset of element 0 in BIN.
    pub base: usize,
    pub stride: usize,
    pub count: usize,
    pub component: Component,
    /// Components per element.
    pub width: usize,
    pub normalized: bool,
    /// No bufferView: every element reads as zero and cannot be written.
    pub zero: bool,
    /// The bufferView index, when the caller passes one through.
    pub view: Option<usize>,
}

impl Layout {
    /// Validate `desc` against a BIN chunk of `bin_len` bytes.
    pub fn resolve(desc: &Desc, bin_len: usize) -> Result<Layout, LayoutError> {
        let component = Component::from_gl(desc.component_type)
            .ok_or(LayoutError::BadComponent(desc.component_type))?;
        let width = type_width(desc.kind).ok_or_else(|| LayoutError::BadType(desc.kind.into()))?;
        let elem = component.size() * width;
        let mut layout = Layout {
            base: 0,
            stride: elem,
            count: desc.count,
            component,
            width,
            normalized: desc.normalized,
            zero: true,
            view: None,
        };
        let Some(view) = desc.view else {
            return Ok(layout);
        };
        if view.buffer != 0 {
            return Err(LayoutError::ExternalBuffer(view.buffer));
        }
        let stride = view.byte_stride.unwrap_or(elem);
        if stride < elem {
            return Err(LayoutError::StrideTooSmall { stride, elem });
        }
        let view_end = view
            .byte_offset
            .checked_add(view.byte_length)
            .ok_or(LayoutError::Overflow)?;
        if view_end > bin_len {
            return Err(LayoutError::ViewPastBin {
                end: view_end,
                bin_len,
            });
        }
        let base = view
            .byte_offset
            .checked_add(desc.byte_offset)
            .ok_or(LayoutError::Overflow)?;
        let end = match desc.count {
            0 => base,
            n => (n - 1)
                .checked_mul(stride)
                .and_then(|span| base.checked_add(span))
                .and_then(|last| last.checked_add(elem))
                .ok_or(LayoutError::Overflow)?,
        };
        if base > view_end || end > view_end {
            return Err(LayoutError::PastView { end, view_end });
        }
        layout.base = base;
        layout.stride = stride;
        layout.zero = false;
        Ok(layout)
    }

    /// Record which bufferView backs this layout (for callers that need
    /// to know whether a view is shared before writing into it).
    pub fn with_view(mut self, view: usize) -> Layout {
        self.view = Some(view);
        self
    }

    fn offset(&self, i: usize, c: usize) -> Option<usize> {
        if i >= self.count || c >= self.width {
            return None;
        }
        self.base
            .checked_add(i.checked_mul(self.stride)?)?
            .checked_add(c.checked_mul(self.component.size())?)
    }

    fn raw(&self, bin: &[u8], i: usize, c: usize) -> Option<Raw> {
        if self.zero {
            return (i < self.count && c < self.width).then_some(Raw::Int(0));
        }
        let o = self.offset(i, c)?;
        let b = bin.get(o..o + self.component.size())?;
        Some(match self.component {
            Component::I8 => Raw::Int(i64::from(b[0] as i8)),
            Component::U8 => Raw::Int(i64::from(b[0])),
            Component::I16 => Raw::Int(i64::from(i16::from_le_bytes([b[0], b[1]]))),
            Component::U16 => Raw::Int(i64::from(u16::from_le_bytes([b[0], b[1]]))),
            Component::U32 => Raw::Int(i64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))),
            Component::F32 => Raw::Float(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        })
    }

    /// Component `c` of element `i` as f64, honouring `normalized` the way
    /// the glTF spec defines it (signed values clamp at -1).
    pub fn read_f64(&self, bin: &[u8], i: usize, c: usize) -> Option<f64> {
        Some(match self.raw(bin, i, c)? {
            Raw::Float(x) => f64::from(x),
            Raw::Int(x) if self.normalized => unorm(self.component, x),
            Raw::Int(x) => x as f64,
        })
    }

    /// Like [`read_f64`](Self::read_f64), but unsigned integers always
    /// read as normalized. For WEIGHTS_n, which the spec requires to be
    /// float or normalized UBYTE/USHORT: a missing `normalized` flag is an
    /// exporter slip, not raw 0-255 weights.
    pub fn read_weight(&self, bin: &[u8], i: usize, c: usize) -> Option<f64> {
        match (self.component, self.raw(bin, i, c)?) {
            (_, Raw::Float(x)) => Some(f64::from(x)),
            (Component::U8 | Component::U16, Raw::Int(x)) => Some(unorm(self.component, x)),
            _ => None,
        }
    }

    /// Unsigned integer components (JOINTS_n, indices) as u32.
    pub fn read_uint(&self, bin: &[u8], i: usize, c: usize) -> Option<u32> {
        match (self.component, self.raw(bin, i, c)?) {
            (Component::U8 | Component::U16 | Component::U32, Raw::Int(x)) => u32::try_from(x).ok(),
            _ => None,
        }
    }

    /// FLOAT components as stored.
    pub fn read_f32(&self, bin: &[u8], i: usize, c: usize) -> Option<f32> {
        match self.raw(bin, i, c)? {
            Raw::Float(x) => Some(x),
            Raw::Int(_) => None,
        }
    }

    /// Write `v` into component `c` of element `i`, quantizing for integer
    /// storage (normalized types scale first). Returns false, leaving the
    /// byte untouched, when the value does not fit the storage or the
    /// layout has no bufferView.
    pub fn write_f64(&self, bin: &mut [u8], i: usize, c: usize, v: f64) -> bool {
        if self.zero || !v.is_finite() {
            return false;
        }
        let Some(o) = self.offset(i, c) else {
            return false;
        };
        let Some(dst) = bin.get_mut(o..o + self.component.size()) else {
            return false;
        };
        if self.component == Component::F32 {
            dst.copy_from_slice(&(v as f32).to_le_bytes());
            return true;
        }
        let (lo, hi) = match self.component {
            Component::I8 => (-128.0, 127.0),
            Component::U8 => (0.0, 255.0),
            Component::I16 => (-32768.0, 32767.0),
            Component::U16 => (0.0, 65535.0),
            _ => (0.0, f64::from(u32::MAX)),
        };
        let scale = match self.component {
            Component::I8 => 127.0,
            Component::U8 => 255.0,
            Component::I16 => 32767.0,
            Component::U16 => 65535.0,
            _ => f64::from(u32::MAX),
        };
        let x = if self.normalized {
            (v * scale).round()
        } else {
            v.round()
        };
        if !(lo..=hi).contains(&x) {
            return false;
        }
        match self.component {
            Component::I8 => dst[0] = x as i8 as u8,
            Component::U8 => dst[0] = x as u8,
            Component::I16 => dst.copy_from_slice(&(x as i16).to_le_bytes()),
            Component::U16 => dst.copy_from_slice(&(x as u16).to_le_bytes()),
            _ => dst.copy_from_slice(&(x as u32).to_le_bytes()),
        }
        true
    }
}

enum Raw {
    Int(i64),
    Float(f32),
}

fn unorm(component: Component, x: i64) -> f64 {
    let x = x as f64;
    match component {
        Component::I8 => (x / 127.0).max(-1.0),
        Component::U8 => x / 255.0,
        Component::I16 => (x / 32767.0).max(-1.0),
        Component::U16 => x / 65535.0,
        _ => x / f64::from(u32::MAX),
    }
}

/// Append `bytes` to `bin` as a new 4-byte-aligned region; returns its
/// offset. Pair with a new bufferView entry in the caller's JSON.
pub fn append_aligned(bin: &mut Vec<u8>, bytes: &[u8]) -> usize {
    bin.resize(bin.len().div_ceil(4) * 4, 0);
    let at = bin.len();
    bin.extend_from_slice(bytes);
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(kind: &str, ct: u64, count: usize, view: Option<View>) -> Desc<'_> {
        Desc {
            count,
            component_type: ct,
            kind,
            normalized: false,
            byte_offset: 0,
            view,
        }
    }

    fn packed(len: usize) -> Option<View> {
        Some(View {
            byte_length: len,
            ..View::default()
        })
    }

    #[test]
    fn reads_packed_floats() {
        let bin: Vec<u8> = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        let l = Layout::resolve(&desc("VEC3", 5126, 2, packed(24)), bin.len()).unwrap();
        assert_eq!(l.read_f64(&bin, 1, 2), Some(6.0));
        assert_eq!(l.read_f32(&bin, 0, 0), Some(1.0));
        assert_eq!(l.read_f64(&bin, 2, 0), None, "past count");
        assert_eq!(l.read_f64(&bin, 0, 3), None, "past width");
    }

    #[test]
    fn interleaved_stride() {
        // POSITION interleaved with a 4-byte tag: stride 16.
        let mut bin = Vec::new();
        for i in 0..3 {
            for c in 0..3 {
                bin.extend_from_slice(&((i * 10 + c) as f32).to_le_bytes());
            }
            bin.extend_from_slice(&[0xAA; 4]);
        }
        let view = Some(View {
            byte_length: 48,
            byte_stride: Some(16),
            ..View::default()
        });
        let l = Layout::resolve(&desc("VEC3", 5126, 3, view), bin.len()).unwrap();
        assert_eq!(l.read_f64(&bin, 2, 1), Some(21.0));
    }

    #[test]
    fn normalized_matches_spec() {
        let bin = [0x81u8, 0x7F, 255, 0];
        let mut d = desc("SCALAR", 5120, 2, packed(2));
        d.normalized = true;
        let l = Layout::resolve(&d, bin.len()).unwrap();
        assert_eq!(l.read_f64(&bin, 0, 0), Some(-1.0));
        assert_eq!(l.read_f64(&bin, 1, 0), Some(1.0));
        let mut d = desc("SCALAR", 5121, 1, packed(4));
        d.byte_offset = 2;
        let l = Layout::resolve(&d, bin.len()).unwrap();
        assert_eq!(l.read_f64(&bin, 0, 0), Some(255.0), "not normalized");
        assert_eq!(l.read_weight(&bin, 0, 0), Some(1.0), "weights always are");
        assert_eq!(l.read_uint(&bin, 0, 0), Some(255));
        assert_eq!(l.read_f32(&bin, 0, 0), None);
    }

    #[test]
    fn missing_view_reads_zeros_and_refuses_writes() {
        let l = Layout::resolve(&desc("VEC4", 5126, 2, None), 0).unwrap();
        assert!(l.zero);
        assert_eq!(l.read_f64(&[], 1, 3), Some(0.0));
        assert_eq!(l.read_f64(&[], 2, 0), None);
        assert!(!l.write_f64(&mut [], 0, 0, 1.0));
    }

    #[test]
    fn bounds_and_shape_errors() {
        let e = |d: Desc, bin_len| Layout::resolve(&d, bin_len).unwrap_err();
        assert_eq!(
            e(desc("VEC3", 1, 1, packed(12)), 12),
            LayoutError::BadComponent(1)
        );
        assert_eq!(
            e(desc("MAT3", 5126, 1, packed(36)), 36),
            LayoutError::BadType("MAT3".into())
        );
        assert_eq!(
            e(desc("VEC3", 5126, 2, packed(12)), 12),
            LayoutError::PastView {
                end: 24,
                view_end: 12
            }
        );
        assert_eq!(
            e(desc("VEC3", 5126, 1, packed(24)), 12),
            LayoutError::ViewPastBin {
                end: 24,
                bin_len: 12
            }
        );
        let view = Some(View {
            byte_length: 12,
            byte_stride: Some(4),
            ..View::default()
        });
        assert_eq!(
            e(desc("VEC3", 5126, 1, view), 12),
            LayoutError::StrideTooSmall {
                stride: 4,
                elem: 12
            }
        );
        let view = Some(View {
            buffer: 1,
            byte_length: 12,
            ..View::default()
        });
        assert_eq!(
            e(desc("VEC3", 5126, 1, view), 12),
            LayoutError::ExternalBuffer(1)
        );
        let mut d = desc("VEC3", 5126, usize::MAX, packed(12));
        d.byte_offset = 0;
        assert_eq!(e(d, 12), LayoutError::Overflow);
    }

    #[test]
    fn write_quantizes_and_refuses_overflow() {
        let mut bin = vec![0u8; 8];
        let mut d = desc("VEC4", 5121, 1, packed(4));
        d.normalized = true;
        let l = Layout::resolve(&d, bin.len()).unwrap();
        assert!(l.write_f64(&mut bin, 0, 0, 0.5));
        assert_eq!(bin[0], 128);
        assert!(!l.write_f64(&mut bin, 0, 1, 1.5), "over 1.0 does not fit");
        assert_eq!(bin[1], 0, "refused writes leave bytes alone");
        let l = Layout::resolve(
            &desc(
                "SCALAR",
                5126,
                1,
                Some(View {
                    byte_offset: 4,
                    byte_length: 4,
                    ..View::default()
                }),
            ),
            8,
        )
        .unwrap();
        assert!(l.write_f64(&mut bin, 0, 0, 0.25));
        assert_eq!(l.read_f64(&bin, 0, 0), Some(0.25));
    }

    #[test]
    fn append_aligns_to_four() {
        let mut bin = vec![1, 2, 3];
        assert_eq!(append_aligned(&mut bin, &[9, 9]), 4);
        assert_eq!(bin, [1, 2, 3, 0, 9, 9]);
    }
}
