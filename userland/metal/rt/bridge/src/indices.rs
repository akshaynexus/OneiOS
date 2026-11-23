// SPDX-License-Identifier: MIT
//! The bridge's texture and sampler indices (metal2vulkan-rt's "Set 1 and index grants": the
//! consumer numbers every texture and sampler a shader reaches). The whole of each class is granted
//! once per device; indices are handed out in turn and given back at a release, and a constexpr
//! sampler keeps one index per state words while a pipeline state numbers it.

use metal2vulkan_runtime as rt;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct Indices {
    inner: Mutex<Inner>,
    _grants: Vec<rt::IndexGrant>,
}

struct Inner {
    next: [u32; 2],
    free: [Vec<u32>; 2],
    constexprs: HashMap<[u64; 2], u32>,
}

fn slot(class: rt::IndexClass) -> usize {
    match class {
        rt::IndexClass::Texture => 0,
        _ => 1,
    }
}

impl Indices {
    /// Grants the whole of both classes on `device`.
    pub fn grant(device: &rt::Device) -> Result<Indices, rt::Error> {
        let mut grants = Vec::new();
        for class in [rt::IndexClass::Texture, rt::IndexClass::Sampler] {
            let range = class.range();
            grants.push(device.grant_indices(class, range.start, range.end - range.start)?);
        }
        Ok(Indices {
            inner: Mutex::new(Inner {
                next: [rt::IndexClass::Texture.range().start, rt::IndexClass::Sampler.range().start],
                free: [Vec::new(), Vec::new()],
                constexprs: HashMap::new(),
            }),
            _grants: grants,
        })
    }

    /// An index of `class` no live object holds, or `None` once all are held.
    pub fn take(&self, class: rt::IndexClass) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let s = slot(class);
        if let Some(index) = inner.free[s].pop() {
            return Some(index);
        }
        let index = inner.next[s];
        if !class.range().contains(&index) {
            return None;
        }
        inner.next[s] += 1;
        Some(index)
    }

    /// Gives an index back once its object's release went to the runtime.
    pub fn give(&self, class: rt::IndexClass, index: u32) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.free[slot(class)].push(index);
    }

    /// The `sampler_ids` answer of a pipeline state's making: one index per constexpr sampler's
    /// state words, the same each time; 0 for the read and write samplers, which take none.
    pub fn sampler_ids(&self, samplers: &[rt::NumberedSampler]) -> Result<Vec<u32>, rt::Error> {
        let mut out = Vec::with_capacity(samplers.len());
        for sampler in samplers {
            out.push(match sampler {
                rt::NumberedSampler::Constexpr(words) => {
                    let known = self.inner.lock().unwrap_or_else(|p| p.into_inner()).constexprs.get(words).copied();
                    match known {
                        Some(index) => index,
                        None => {
                            let index = self.take(rt::IndexClass::Sampler).unwrap_or(0);
                            self.inner.lock().unwrap_or_else(|p| p.into_inner()).constexprs.insert(*words, index);
                            index
                        }
                    }
                }
                _ => 0,
            });
        }
        Ok(out)
    }
}
