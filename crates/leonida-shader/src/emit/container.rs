//! The PC container around emitted DXIL: feature flags (`SFI0`), the
//! signatures (`ISG1`, `OSG1`), pipeline state validation (`PSV0`) and the
//! program (`DXIL`), laid out as validator version 1.6 lays them out, plus
//! the container hash that D3D12 checks.

/// One signature element: one row (D3D12 links stages by semantic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SigElement {
    pub name: String,
    pub index: u32,
    /// DXIL semantic kind (0 arbitrary, 1 vertex id, 3 position, 16 target...).
    pub kind: u8,
    /// DXIL component type (4 i32, 5 u32, 9 f32, 1 i1).
    pub comp: u8,
    /// DXIL interpolation mode (0 for none).
    pub interp: u8,
    /// Start row, if the element has a register.
    pub row: Option<u32>,
    pub col: u8,
    pub cols: u8,
    /// Components read (inputs) or written (outputs), relative to `col`.
    pub usage: u8,
    /// 16-bit on the console (declared 32-bit here).
    pub half: bool,
}

impl SigElement {
    fn system_value(&self) -> u32 {
        match self.kind {
            1 => 6,  // vertex id
            2 => 8,  // instance id
            3 => 1,  // position
            4 => 4,  // render target array index
            5 => 5,  // viewport array index
            6 => 2,  // clip distance
            7 => 3,  // cull distance
            10 => 7, // primitive id
            12 => 10,
            13 => 9, // front face
            14 => 66,
            15 => 70,
            16 => 64, // target
            17 => 65, // depth
            18 => 68,
            19 => 67,
            20 => 69,
            28 => 23, // barycentrics
            30 => 25, // cull primitive
            _ => 0,
        }
    }

    fn sig_comp(&self) -> u32 {
        match self.comp {
            1 | 5 => 1, // bool and u32
            4 => 2,
            9 => 3,
            _ => 0,
        }
    }

    fn mask(&self) -> u8 {
        ((1u8 << self.cols) - 1) << self.col
    }
}

/// `ISG1`/`OSG1`: 32-byte rows, then the names.
pub(crate) fn signature(elements: &[SigElement], input: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend((elements.len() as u32).to_le_bytes());
    out.extend(8u32.to_le_bytes());
    // Validator 1.6 writes one name per element in element order, repeats
    // included, except that system values share theirs (it keys names by
    // the semantic's string, which is one static string per system value);
    // the rows sorted by register (unallocated last), then by name offset: in
    // element order within a register, not by column.
    let mut offsets: Vec<u32> = Vec::new();
    let mut next = 8 + 32 * elements.len() as u32;
    let mut names = Vec::new();
    for (i, e) in elements.iter().enumerate() {
        let shared = (e.kind != 0)
            .then(|| (0..i).find(|&j| elements[j].kind == e.kind))
            .flatten();
        match shared {
            Some(j) => offsets.push(offsets[j]),
            None => {
                offsets.push(next);
                next += e.name.len() as u32 + 1;
                names.push(&e.name);
            }
        }
    }
    let mut order: Vec<usize> = (0..elements.len()).collect();
    order.sort_by_key(|&i| elements[i].row.unwrap_or(u32::MAX));
    for i in order {
        let (e, offset) = (&elements[i], offsets[i]);
        let used = e.usage << e.col;
        let rw = if input { used } else { !used & 0xf };
        for v in [
            0,
            offset,
            e.index,
            e.system_value(),
            e.sig_comp(),
            e.row.unwrap_or(u32::MAX),
        ] {
            out.extend(v.to_le_bytes());
        }
        out.extend([e.mask(), rw, 0, 0]);
        out.extend(0u32.to_le_bytes());
    }
    for name in names {
        out.extend(name.as_bytes());
        out.push(0);
    }
    out
}

/// The start register, start column and interpolation mode `PSV0` (runtime
/// info version 1 or later) gives each allocated element of signature
/// `which` (0 inputs, 1 outputs, 2 patch constants or primitives); `None`
/// if the part lists no signature elements or is cut short.
pub(crate) fn psv_modes(psv: &[u8], which: usize) -> Option<Vec<(u32, u8, u8)>> {
    let u32_at = |at: usize| -> Option<usize> {
        let b = psv.get(at..at.checked_add(4)?)?;
        Some(u32::from_le_bytes(b.try_into().ok()?) as usize)
    };
    let info = u32_at(0)?;
    if info < 36 || which > 2 {
        return None;
    }
    let counts: Vec<usize> = psv.get(4 + 28..4 + 31)?.iter().map(|&n| n.into()).collect();
    if counts.iter().sum::<usize>() == 0 {
        return None;
    }
    let mut at = 4 + info;
    let resources = u32_at(at)?;
    at += 4;
    if resources > 0 {
        at += 4 + u32_at(at)?.checked_mul(resources)?;
    }
    at += 4 + u32_at(at)?;
    at += 4 + 4 * u32_at(at)?;
    let size = u32_at(at)?;
    if size < 14 {
        return None;
    }
    let first = at + 4 + size * counts[..which].iter().sum::<usize>();
    let mut out = Vec::new();
    for k in 0..counts[which] {
        let e = psv.get(first + size * k..first + size * k + 14)?;
        if e[10] & 0x40 != 0 {
            out.push((u32::from(e[9]), (e[10] >> 4) & 3, e[13]));
        }
    }
    Some(out)
}

/// Each element of `elements` takes the interpolation mode `PSV0` gives the
/// element at its register and column in signature `which`, if any.
pub(crate) fn take_modes(psv: &[u8], which: usize, elements: &mut [SigElement]) {
    let Some(modes) = psv_modes(psv, which) else {
        return;
    };
    for e in elements {
        if let Some(&(_, _, mode)) = modes
            .iter()
            .find(|&&(row, col, _)| e.row == Some(row) && e.col == col)
            && mode != 0
        {
            e.interp = mode;
        }
    }
}

/// Each unread arbitrary element takes the interpolation mode of the read
/// elements packed into its register, if they agree on one (an integer only
/// a constant one): the validator wants one mode per register, and an input
/// the program reads per vertex is constant beside neighbours `PSV0` gave
/// the console's linear mode.
pub(crate) fn share_row_modes(elements: &mut [SigElement]) {
    let modes: Vec<Option<u8>> = elements
        .iter()
        .map(|e| {
            let row = e.row?;
            if e.usage != 0 || e.kind != 0 {
                return None;
            }
            let mut read = elements
                .iter()
                .filter(|o| o.row == Some(row) && o.usage != 0)
                .map(|o| o.interp);
            let mode = read.next()?;
            if read.any(|m| m != mode) || (mode != 1 && e.comp != 9) {
                return None;
            }
            Some(mode)
        })
        .collect();
    for (e, mode) in elements.iter_mut().zip(modes) {
        if let Some(mode) = mode {
            e.interp = mode;
        }
    }
}

/// The most a mesh shader's outputs may take, in bytes (D3D12).
pub(crate) const MESH_OUTPUT_LIMIT: u64 = 32 * 1024;

/// A mesh shader's output size as the DXIL validator measures it against
/// [`MESH_OUTPUT_LIMIT`]: 4 bytes per component of every vertex element
/// and primitive element, times the maxima rounded up to multiples of 32.
pub(crate) fn mesh_output_bytes(
    vertex: &[SigElement],
    primitive: &[SigElement],
    max_vertices: u32,
    max_primitives: u32,
) -> u64 {
    let scalars = |els: &[SigElement]| els.iter().map(|e| u64::from(e.cols)).sum::<u64>();
    let align = |n: u32| u64::from(n).next_multiple_of(32);
    4 * (scalars(vertex) * align(max_vertices) + scalars(primitive) * align(max_primitives))
}

/// A resource bound by register, as `PSV0` lists it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Binding {
    /// PSV resource type: 1 sampler, 2 CBV, 4 raw SRV, 7 raw UAV.
    pub kind: u32,
    pub space: u32,
    pub register: u32,
    /// DXIL resource kind (13 cbuffer, 14 sampler).
    pub dxil_kind: u32,
}

pub(crate) struct Psv<'a> {
    /// PSV shader kind: 0 pixel, 1 vertex, 2 geometry, 5 compute.
    pub stage: u8,
    /// Vertex: writes `SV_Position`. Pixel: writes depth.
    pub flag: bool,
    pub inputs: &'a [SigElement],
    pub outputs: &'a [SigElement],
    /// Mesh: per-primitive outputs.
    pub prims: &'a [SigElement],
    /// Mesh: its state and the group-shared bytes it declares.
    pub mesh: Option<(super::shader::Mesh, u32)>,
    /// Geometry (triangles in, triangle strips out on stream 0): its
    /// maximum vertex count. `flag`: writes `SV_Position`.
    pub geometry: Option<u32>,
    pub bindings: &'a [Binding],
    /// Compute: thread-group size and required wave size.
    pub threads: [u32; 3],
    pub wave_size: Option<u32>,
}

/// `PSV0` with runtime info version 2 (validator 1.6).
pub(crate) fn psv(p: &Psv) -> Vec<u8> {
    let vectors = |els: &[SigElement]| {
        els.iter()
            .filter_map(|e| e.row)
            .map(|r| r + 1)
            .max()
            .unwrap_or(0)
    };
    let (in_vectors, out_vectors) = (vectors(p.inputs), vectors(p.outputs));
    let mut out = Vec::new();
    let u32s = |out: &mut Vec<u8>, vs: &[u32]| {
        for v in vs {
            out.extend(v.to_le_bytes());
        }
    };
    u32s(&mut out, &[48]);
    let mut info = [0u8; 48];
    info[0] = u8::from(p.flag);
    if p.stage == 0 {
        // PSInfo.SampleFrequency: an input interpolated per sample, or the
        // sample index.
        info[1] = u8::from(
            p.inputs
                .iter()
                .any(|e| matches!(e.interp, 6 | 7) || e.kind == 12),
        );
    }
    let (min, max) = p.wave_size.map_or((0, u32::MAX), |n| (n, n));
    info[16..20].copy_from_slice(&min.to_le_bytes());
    info[20..24].copy_from_slice(&max.to_le_bytes());
    info[24] = p.stage;
    for (k, n) in p.threads.into_iter().enumerate() {
        info[36 + 4 * k..40 + 4 * k].copy_from_slice(&n.to_le_bytes());
    }
    info[28] = p.inputs.len() as u8;
    info[29] = p.outputs.len() as u8;
    info[31] = in_vectors as u8;
    info[32] = out_vectors as u8;
    if let Some((mesh, shared)) = p.mesh {
        info[0..4].copy_from_slice(&shared.to_le_bytes());
        info[12..14].copy_from_slice(&(mesh.max_vertices as u16).to_le_bytes());
        info[14..16].copy_from_slice(&(mesh.max_primitives as u16).to_le_bytes());
        info[26] = vectors(p.prims) as u8;
        info[27] = mesh.topology;
        info[30] = p.prims.len() as u8;
    }
    if let Some(max_vertices) = p.geometry {
        // Input primitive (3: triangles), output topology (5: triangle
        // strips), stream mask, position written; the vertex maximum.
        info[0..4].copy_from_slice(&3u32.to_le_bytes());
        info[4..8].copy_from_slice(&5u32.to_le_bytes());
        info[8..12].copy_from_slice(&1u32.to_le_bytes());
        info[12] = u8::from(p.flag);
        info[26..28].copy_from_slice(&(max_vertices as u16).to_le_bytes());
    }
    out.extend(info);
    u32s(&mut out, &[p.bindings.len() as u32]);
    if !p.bindings.is_empty() {
        u32s(&mut out, &[24]);
        for b in p.bindings {
            u32s(
                &mut out,
                &[b.kind, b.space, b.register, b.register, b.dxil_kind, 0],
            );
        }
    }
    // Names of arbitrary semantics (system values use ""), then indices.
    let mut strings = vec![0u8];
    let mut indices: Vec<u32> = Vec::new();
    let mut elements = Vec::new();
    for e in p.inputs.iter().chain(p.outputs).chain(p.prims) {
        let name = if e.kind == 0 {
            let needle: Vec<u8> = e.name.bytes().chain([0]).collect();
            match find(&strings, &needle) {
                Some(at) => at,
                None => {
                    strings.extend(&needle);
                    strings.len() - needle.len()
                }
            }
        } else {
            0
        };
        let index = match indices.iter().position(|&i| i == e.index) {
            Some(at) => at,
            None => {
                indices.push(e.index);
                indices.len() - 1
            }
        };
        let (start, alloc) = match e.row {
            Some(r) => (r as u8, 0x40 | (e.col << 4)),
            None => (0, 0),
        };
        elements.push((name as u32, index as u32, start, e.cols | alloc, e));
    }
    strings.resize(strings.len().next_multiple_of(4), 0);
    u32s(&mut out, &[strings.len() as u32]);
    out.extend(&strings);
    u32s(&mut out, &[indices.len() as u32]);
    u32s(&mut out, &indices);
    if !elements.is_empty() {
        u32s(&mut out, &[16]);
        for (name, index, start, cols, e) in elements {
            u32s(&mut out, &[name, index]);
            out.extend([1, start, cols, e.kind, e.sig_comp() as u8, e.interp, 0, 0]);
        }
    }
    // Input-to-output dependencies, as the view-id state declares them:
    // every output scalar on every input scalar.
    if in_vectors > 0 && out_vectors > 0 {
        let scalars = |els: &[SigElement]| {
            els.iter()
                .filter_map(|e| e.row.map(|r| r * 4 + u32::from(e.col + e.cols)))
                .max()
                .unwrap_or(0)
        };
        let (ni, no) = (scalars(p.inputs), scalars(p.outputs));
        let per = (out_vectors * 4).div_ceil(32);
        for i in 0..in_vectors * 4 {
            for k in 0..per {
                let bits = if i < ni {
                    no.saturating_sub(32 * k).min(32)
                } else {
                    0
                };
                let mask = if bits == 32 {
                    u32::MAX
                } else {
                    (1u32 << bits) - 1
                };
                u32s(&mut out, &[mask]);
            }
        }
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The container: parts in order, hashed.
pub(crate) fn build(parts: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let table = 0x20 + 4 * parts.len();
    let total = table + parts.iter().map(|(_, d)| 8 + d.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend(b"DXBC");
    out.extend([0; 16]);
    out.extend(1u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((parts.len() as u32).to_le_bytes());
    let mut offset = table;
    for (_, d) in parts {
        out.extend((offset as u32).to_le_bytes());
        offset += 8 + d.len();
    }
    for (fourcc, d) in parts {
        out.extend(*fourcc);
        out.extend((d.len() as u32).to_le_bytes());
        out.extend(*d);
    }
    let digest = hash(&out[20..]);
    out[4..20].copy_from_slice(&digest);
    out
}

/// The container hash D3D12 checks: MD5 whose last block carries the bit
/// count first and `2 × bytes + 1` last instead of standard padding.
pub(crate) fn hash(data: &[u8]) -> [u8; 16] {
    let len = data.len();
    let left = len % 64;
    let mut tail = Vec::with_capacity(128);
    if left < 56 {
        tail.extend(((len as u32) << 3).to_le_bytes());
        tail.extend(&data[len - left..]);
        tail.push(0x80);
        tail.resize(60, 0);
    } else {
        tail.extend(&data[len - left..]);
        tail.push(0x80);
        tail.resize(64, 0);
        tail.extend(((len as u32) << 3).to_le_bytes());
        tail.resize(124, 0);
    }
    tail.extend(((len as u32) << 1 | 1).to_le_bytes());
    let mut state = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in data[..len - left]
        .as_chunks::<64>()
        .0
        .iter()
        .chain(tail.as_chunks::<64>().0)
    {
        md5_block(&mut state, block);
    }
    let mut out = [0; 16];
    for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        o.copy_from_slice(&s.to_le_bytes());
    }
    out
}

fn md5_block(state: &mut [u32; 4], block: &[u8; 64]) {
    const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    const K: [u32; 64] = [
        0xd76a_a478, 0xe8c7_b756, 0x2420_70db, 0xc1bd_ceee,
        0xf57c_0faf, 0x4787_c62a, 0xa830_4613, 0xfd46_9501,
        0x6980_98d8, 0x8b44_f7af, 0xffff_5bb1, 0x895c_d7be,
        0x6b90_1122, 0xfd98_7193, 0xa679_438e, 0x49b4_0821,
        0xf61e_2562, 0xc040_b340, 0x265e_5a51, 0xe9b6_c7aa,
        0xd62f_105d, 0x0244_1453, 0xd8a1_e681, 0xe7d3_fbc8,
        0x21e1_cde6, 0xc337_07d6, 0xf4d5_0d87, 0x455a_14ed,
        0xa9e3_e905, 0xfcef_a3f8, 0x676f_02d9, 0x8d2a_4c8a,
        0xfffa_3942, 0x8771_f681, 0x6d9d_6122, 0xfde5_380c,
        0xa4be_ea44, 0x4bde_cfa9, 0xf6bb_4b60, 0xbebf_bc70,
        0x289b_7ec6, 0xeaa1_27fa, 0xd4ef_3085, 0x0488_1d05,
        0xd9d4_d039, 0xe6db_99e5, 0x1fa2_7cf8, 0xc4ac_5665,
        0xf429_2244, 0x432a_ff97, 0xab94_23a7, 0xfc93_a039,
        0x655b_59c3, 0x8f0c_cc92, 0xffef_f47d, 0x8584_5dd1,
        0x6fa8_7e4f, 0xfe2c_e6e0, 0xa301_4314, 0x4e08_11a1,
        0xf753_7e82, 0xbd3a_f235, 0x2ad7_d2bb, 0xeb86_d391,
    ];
    let x: [u32; 16] = std::array::from_fn(|i| {
        u32::from_le_bytes([
            block[4 * i],
            block[4 * i + 1],
            block[4 * i + 2],
            block[4 * i + 3],
        ])
    });
    let [mut a, mut b, mut c, mut d] = *state;
    for (i, &k) in K.iter().enumerate() {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let rotated = a
            .wrapping_add(f)
            .wrapping_add(k)
            .wrapping_add(x[g])
            .rotate_left(S[(i / 16) * 4 + i % 4]);
        (a, b, c, d) = (d, b.wrapping_add(rotated), b, c);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d]) {
        *s = s.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_core_matches_the_standard() {
        // Standard MD5 of "abc" with standard padding, through md5_block.
        let mut block = b"abc".to_vec();
        block.push(0x80);
        block.resize(56, 0);
        block.extend(24u64.to_le_bytes());
        let mut state = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
        md5_block(&mut state, block.as_slice().try_into().unwrap());
        let hex: String = state
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, "900150983cd24fb0d6963f7d28e17f72");
    }

    fn element(name: &str, kind: u8, interp: u8, row: u32, col: u8, cols: u8) -> SigElement {
        SigElement {
            name: name.into(),
            index: 0,
            kind,
            comp: 9,
            interp,
            row: Some(row),
            col,
            cols,
            usage: (1 << cols) - 1,
            half: false,
        }
    }

    #[test]
    fn rows_of_one_register_keep_element_order() {
        // As the console's containers list them: column 3 before column 0.
        let els = [
            element("TEXCOORD", 0, 2, 0, 3, 1),
            element("TANGENT", 0, 2, 0, 0, 3),
        ];
        let part = signature(&els, false);
        let name_offset =
            |row: usize| u32::from_le_bytes(part[8 + 32 * row + 4..][..4].try_into().unwrap());
        assert_eq!(name_offset(0), 8 + 64);
        assert_eq!(part[8 + 24], 0b1000);
    }

    fn psv_element(interp: u8, comp: u8, row: u32, col: u8, cols: u8) -> SigElement {
        SigElement {
            name: "TEXCOORD".into(),
            index: row,
            kind: 0,
            comp,
            interp,
            row: Some(row),
            col,
            cols,
            usage: (1 << cols) - 1,
            half: false,
        }
    }

    #[test]
    fn elements_take_the_modes_psv0_gives_their_register() {
        // As the console packs them: a constant float beside a constant
        // uint, which the default table would make linear.
        let console = [
            psv_element(2, 9, 1, 0, 4),
            psv_element(1, 9, 7, 0, 1),
            psv_element(1, 5, 7, 1, 1),
        ];
        let part = psv(&Psv {
            stage: 1,
            flag: false,
            inputs: &[],
            outputs: &console,
            prims: &[],
            mesh: None,
            geometry: None,
            bindings: &[],
            threads: [0; 3],
            wave_size: None,
        });
        let mut ours = console.clone();
        for e in &mut ours {
            e.interp = if e.comp == 9 { 2 } else { 1 };
        }
        take_modes(&part, 0, &mut ours);
        assert_eq!(ours[1].interp, 2);
        take_modes(&part, 1, &mut ours);
        assert_eq!(ours, console);
        // A part cut short leaves the modes alone.
        assert_eq!(psv_modes(&part[..40], 1), None);
    }

    #[test]
    fn unread_elements_take_the_mode_read_in_their_register() {
        // As in PS_RasterizeVehicle*InterfaceDepth: TEXCOORD3 (row 5, w)
        // read per vertex, so constant, beside an unread linear NORMAL.
        let mut normal = psv_element(2, 9, 5, 0, 3);
        normal.usage = 0;
        // A register read two ways is left alone, and an unread uint never
        // turns linear.
        let mut mixed = psv_element(2, 9, 2, 2, 1);
        mixed.usage = 0;
        let mut uint = psv_element(1, 5, 3, 2, 1);
        uint.usage = 0;
        let mut els = [
            normal,
            psv_element(1, 9, 5, 3, 1),
            psv_element(2, 9, 2, 0, 1),
            psv_element(1, 9, 2, 1, 1),
            mixed,
            psv_element(2, 9, 3, 0, 2),
            uint,
        ];
        share_row_modes(&mut els);
        let modes: Vec<u8> = els.iter().map(|e| e.interp).collect();
        assert_eq!(modes, [1, 1, 2, 1, 2, 2, 1]);
    }
}
