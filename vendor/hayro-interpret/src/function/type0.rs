use crate::function::ParsedFunction;
use crate::function::{Clamper, TupleVec, Values, interpolate};
use hayro_syntax::bit_reader::BitReader;
use hayro_syntax::object::Array;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::{BITS_PER_SAMPLE, DECODE, ENCODE, SIZE};
use smallvec::{SmallVec, ToSmallVec, smallvec};
use std::collections::HashMap;

/// A type 0 function (sampled function).
#[derive(Debug)]
pub(crate) struct Type0 {
    sizes: IntVec,
    table: HashMap<Key, IntVec>,
    clamper: Clamper,
    range: TupleVec,
    bits_per_sample: u8,
    encode: TupleVec,
    decode: TupleVec,
}

impl Type0 {
    /// Create a new type 0 function.
    pub(crate) fn new(stream: &Stream<'_>) -> Option<Self> {
        let dict = stream.dict();
        let bits_per_sample = dict.get::<u8>(BITS_PER_SAMPLE)?;

        if !matches!(bits_per_sample, 1 | 2 | 4 | 8 | 16 | 24 | 32) {
            error!("invalid bits per sample: {bits_per_sample}");

            return None;
        }

        let clamper = Clamper::new(dict)?;
        let range = clamper.range.clone()?;

        if range.is_empty() {
            warn!("encountered Type0 function with invalid range length 0.");

            return None;
        }

        let sizes = dict
            .get::<Array<'_>>(SIZE)?
            .iter::<u32>()
            .collect::<IntVec>();

        let encode = dict.get::<TupleVec>(ENCODE).unwrap_or(
            sizes
                .iter()
                .map(|s| (0.0, s.saturating_sub(1) as f32))
                .collect(),
        );

        // PdfCraft patch: see `MAX_SAMPLE_VALUES`.
        let Some(num_expected_entries) = sizes
            .iter()
            .try_fold(1usize, |n, s| n.checked_mul(*s as usize))
            .filter(|entries| *entries <= MAX_SAMPLE_ENTRIES)
            .and_then(|entries| entries.checked_mul(range.len()))
            .filter(|n| *n <= MAX_SAMPLE_VALUES)
        else {
            warn!("Type0 function has too many samples");

            return None;
        };

        let decode = dict.get::<TupleVec>(DECODE).unwrap_or(range.clone());

        let mut data = {
            let decoded = stream.decoded().ok()?;
            let mut buf = vec![];
            let mut reader = BitReader::new(&decoded);

            while buf.len() < num_expected_entries
                && let Some(data) = reader.read(bits_per_sample)
            {
                buf.push(data);
            }

            buf
        };

        if data.len() != num_expected_entries {
            warn!("Type0 function didn't have the expected number of sample entries.");
            data.truncate(num_expected_entries);
        }

        let table = build_table(&data, &sizes, range.len())?;

        Some(Self {
            sizes,
            clamper,
            range,
            bits_per_sample,
            table,
            encode,
            decode,
        })
    }

    /// Evaluate a type 0 function with the given input.
    pub(crate) fn eval(&self, mut input: Values) -> Option<Values> {
        if input.len() != self.sizes.len() {
            warn!("wrong number of arguments for sampled function");

            return None;
        }

        self.clamper.clamp_input(&mut input);

        let mut key = input;

        for (((x, domain), encode), size) in key
            .iter_mut()
            .zip(self.clamper.domain.iter())
            .zip(self.encode.iter())
            .zip(self.sizes.iter())
        {
            *x = interpolate(*x, domain.0, domain.1, encode.0, encode.1);
            *x = x.max(0.0).min(*size as f32 - 1.0);
        }

        let in_prev = key.iter().map(|v| v.floor() as u32).collect::<IntVec>();
        let in_next = key.iter().map(|v| v.ceil() as u32).collect::<IntVec>();

        let interpolator = Interpolator::new(
            key.clone().to_smallvec(),
            in_prev,
            in_next,
            self.sizes.clone(),
            self.range.len(),
        );

        let interpolated = interpolator.interpolate(&self.table)?;

        let mut out = interpolated
            .iter()
            .zip(self.decode.iter())
            .map(|(x, decode)| {
                interpolate(
                    *x,
                    0.0,
                    (2_u32.pow(self.bits_per_sample as u32) - 1) as f32,
                    decode.0,
                    decode.1,
                )
            })
            .collect::<SmallVec<_>>();

        self.clamper.clamp_output(&mut out);

        Some(out)
    }

    /// PdfCraft patch (ps-export): snapshot for re-emission (see `Function::parsed`).
    /// The sample table is walked in stream order — the same first-dimension-fastest
    /// odometer `build_table` filled it with.
    pub(crate) fn parsed(&self) -> ParsedFunction {
        ParsedFunction::Sampled {
            domain: self.clamper.domain.iter().copied().collect(),
            range: self.range.iter().copied().collect(),
            sizes: self.sizes.iter().copied().collect(),
            bits_per_sample: self.bits_per_sample,
            encode: self.encode.iter().copied().collect(),
            decode: self.decode.iter().copied().collect(),
            samples: self.ordered_samples(),
        }
    }

    /// Every sample value in stream order; entries missing from a truncated stream
    /// read as 0. Sizes of 0 yield no samples at all.
    fn ordered_samples(&self) -> Vec<u32> {
        let total = self
            .sizes
            .iter()
            .try_fold(1usize, |acc, &s| acc.checked_mul(s as usize))
            .unwrap_or(0);
        let comps = self.range.len();
        let mut samples = Vec::with_capacity(total.saturating_mul(comps).min(1 << 23));
        let mut parts: IntVec = smallvec![0; self.sizes.len()];

        for _ in 0..total {
            let key = Key::from_raw(&self.sizes, &parts);
            let entry = self.table.get(&key);
            for c in 0..comps {
                samples.push(entry.and_then(|e| e.get(c).copied()).unwrap_or(0));
            }

            // Advance the odometer: dimension 0 varies fastest.
            for i in 0..parts.len() {
                let size = self.sizes.get(i).copied().unwrap_or(1).max(1) as usize;
                if let Some(part) = parts.get_mut(i) {
                    *part += 1;
                    if (*part as usize) < size {
                        break;
                    }
                    *part = 0;
                }
            }
        }

        samples
    }
}

type FloatVec = SmallVec<[f32; 4]>;
type IntVec = SmallVec<[u32; 4]>;

// See <https://github.com/apache/pdfbox/blob/bb778d4784f354c36ce032e91a0cee2169a4c598/pdfbox/src/main/java/org/apache/pdfbox/pdmodel/common/function/PDFunctionType0.java#L252>
struct Interpolator {
    input: FloatVec,
    sizes: IntVec,
    in_prev: IntVec,
    in_next: IntVec,
    out_len: usize,
}

impl Interpolator {
    fn new(
        input: FloatVec,
        in_prev: IntVec,
        in_next: IntVec,
        sizes: IntVec,
        out_len: usize,
    ) -> Self {
        Self {
            input,
            in_prev,
            in_next,
            sizes,
            out_len,
        }
    }

    fn interpolate(&self, table: &HashMap<Key, IntVec>) -> Option<FloatVec> {
        self.interpolate_inner(smallvec![0; self.input.len()], 0, table)
    }

    fn interpolate_inner(
        &self,
        mut coord: IntVec,
        step: usize,
        table: &HashMap<Key, IntVec>,
    ) -> Option<FloatVec> {
        if step == self.input.len() - 1 {
            if self.in_prev[step] == self.in_next[step] {
                coord[step] = self.in_prev[step];

                Some(
                    table
                        .get(&Key::from_raw(&self.sizes, &coord))?
                        .clone()
                        .iter()
                        .map(|n| *n as f32)
                        .collect(),
                )
            } else {
                coord[step] = self.in_prev[step];
                let val1 = table.get(&Key::from_raw(&self.sizes, &coord))?;
                coord[step] = self.in_next[step];
                let val2 = table.get(&Key::from_raw(&self.sizes, &coord))?;
                let mut out = smallvec![0.0; self.out_len];

                for i in 0..self.out_len {
                    out[i] = interpolate(
                        self.input[step],
                        self.in_prev[step] as f32,
                        self.in_next[step] as f32,
                        val1[i] as f32,
                        val2[i] as f32,
                    );
                }

                Some(out)
            }
        } else if self.in_prev[step] == self.in_next[step] {
            coord[step] = self.in_prev[step];
            self.interpolate_inner(coord, step + 1, table)
        } else {
            coord[step] = self.in_prev[step];
            let val1 = self.interpolate_inner(coord.clone(), step + 1, table)?;
            coord[step] = self.in_next[step];
            let val2 = self.interpolate_inner(coord, step + 1, table)?;

            let mut out = smallvec![0.0; self.out_len];

            for i in 0..self.out_len {
                out[i] = interpolate(
                    self.input[step],
                    self.in_prev[step] as f32,
                    self.in_next[step] as f32,
                    val1[i],
                    val2[i],
                );
            }

            Some(out)
        }
    }
}

/// PdfCraft patch: the most sample values a sampled function may have, and the most table
/// entries (one per point of its `/Size` grid). The samples were read into 4-byte values and
/// then into a table entry each, so a stream inflating to 256 MiB of 1-bit samples asked for
/// 8 GB; `/Size` sets how many the table expects. The largest real tables are DeviceN tint
/// transforms: 33 × 33 × 33 × 33 points with 3 outputs is 1.2 million entries and 3.6 million
/// values, 17^5 points with 4 outputs 5.7 million values.
const MAX_SAMPLE_VALUES: usize = 1 << 23;
const MAX_SAMPLE_ENTRIES: usize = 1 << 21;

fn build_table(data: &[u32], sizes: &[u32], n: usize) -> Option<HashMap<Key, IntVec>> {
    let mut key = Key::new(sizes);
    let mut table = HashMap::new();

    let mut first = true;

    for b in data.chunks_exact(n) {
        if !first {
            key.increment();
        }

        table.insert(key.clone(), b.to_smallvec());

        first = false;
    }

    Some(table)
}

/// A sampled function consists of a (possibly) multi-dimensional table that we can index
/// into. We do this by representing the entries as a flat list of vectors, where each
/// element in the vector represents the value of the key in that specific dimension.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    sizes: SmallVec<[u32; 4]>,
    parts: SmallVec<[u32; 4]>,
}

impl Key {
    fn new(sizes: &[u32]) -> Self {
        let parts = smallvec![0; sizes.len()];

        Self {
            sizes: sizes.to_smallvec(),
            parts,
        }
    }

    fn from_raw(sizes: &[u32], parts: &[u32]) -> Self {
        Self {
            sizes: sizes.to_smallvec(),
            parts: parts.to_smallvec(),
        }
    }

    fn increment(&mut self) -> Option<()> {
        self.increment_index(0)
    }

    fn increment_index(&mut self, index: usize) -> Option<()> {
        let size = *self.sizes.get(index).or_else(|| {
            error!("overflowed key in sampled function");

            None
        })?;
        let val = self.parts.get_mut(index)?;

        if *val >= (size - 1) {
            *val = 0;
            self.increment_index(index + 1)?;
        } else {
            *val += 1;
        }

        Some(())
    }
}
