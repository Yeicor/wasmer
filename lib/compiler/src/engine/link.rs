//! Linking for Universal-compiled code.

use crate::{
    get_libcall_trampoline,
    types::{
        function::GOT,
        relocation::{RelocationKind, RelocationLike, RelocationTarget},
        section::SectionIndex,
    },
    FunctionExtent,
};
use std::{
    collections::{HashMap, HashSet},
    ptr::{read_unaligned, write_unaligned},
};

use wasmer_types::{entity::PrimaryMap, LocalFunctionIndex, ModuleInfo};
use wasmer_vm::{libcalls::function_pointer, SectionBodyPtr};

fn apply_relocation(
    body: usize,
    r: &impl RelocationLike,
    allocated_functions: &PrimaryMap<LocalFunctionIndex, FunctionExtent>,
    allocated_sections: &PrimaryMap<SectionIndex, SectionBodyPtr>,
    libcall_trampolines_sec_idx: SectionIndex,
    libcall_trampoline_len: usize,
    riscv_pcrel_hi20s: &mut HashMap<usize, u32>,
    got_info: &Option<(usize, GOT)>,
) {
    let reloc_target = r.reloc_target();
    let target_func_address: usize = match reloc_target {
        RelocationTarget::LocalFunc(index) => *allocated_functions[index].ptr as usize,
        RelocationTarget::LibCall(libcall) => {
            // Use the direct target of the libcall if the relocation supports
            // a full 64-bit address. Otherwise use a trampoline.
            if matches!(
                r.kind(),
                RelocationKind::Abs8
                    | RelocationKind::X86PCRel8
                    | RelocationKind::MachoArm64RelocUnsigned
                    | RelocationKind::MachoX86_64RelocUnsigned
                    | RelocationKind::MachoArm64RelocGotLoadPage21
                    | RelocationKind::MachoArm64RelocGotLoadPageoff12
                    | RelocationKind::MachoArm64RelocPointerToGot
            ) {
                function_pointer(libcall)
            //} else if matches!(r.kind(), RelocationKind::MachoArm64RelocPointerToGot) {
            //    Box::leak(Box::new(function_pointer(libcall))) as *mut _ as usize
            } else {
                get_libcall_trampoline(
                    libcall,
                    allocated_sections[libcall_trampolines_sec_idx].0 as usize,
                    libcall_trampoline_len,
                )
            }
        }
        RelocationTarget::CustomSection(custom_section) => {
            *allocated_sections[custom_section] as usize
        }
    };

    // A set of addresses at which a SUBTRACTOR relocation was applied.
    let mut macho_aarch64_subtractor_addresses = HashSet::new();

    match r.kind() {
        RelocationKind::Abs8 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            write_unaligned(reloc_address as *mut u64, reloc_delta);
        },
        RelocationKind::X86PCRel4 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            write_unaligned(reloc_address as *mut u32, reloc_delta as _);
        },
        RelocationKind::X86PCRel8 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            write_unaligned(reloc_address as *mut u64, reloc_delta);
        },
        RelocationKind::X86CallPCRel4 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            write_unaligned(reloc_address as *mut u32, reloc_delta as _);
        },
        RelocationKind::Arm64Call => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            if (reloc_delta as i64).abs() >= 0x1000_0000 {
                panic!(
                    "Relocation to big for {:?} for {:?} with {:x}, current val {:x}",
                    r.kind(),
                    r.reloc_target(),
                    reloc_delta,
                    read_unaligned(reloc_address as *mut u32)
                )
            }
            let reloc_delta = (((reloc_delta / 4) as u32) & 0x3ff_ffff)
                | (read_unaligned(reloc_address as *mut u32) & 0xfc00_0000);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::Arm64Movw0 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let reloc_delta =
                (((reloc_delta & 0xffff) as u32) << 5) | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::Arm64Movw1 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let reloc_delta = ((((reloc_delta >> 16) & 0xffff) as u32) << 5)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::Arm64Movw2 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let reloc_delta = ((((reloc_delta >> 32) & 0xffff) as u32) << 5)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::Arm64Movw3 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let reloc_delta = ((((reloc_delta >> 48) & 0xffff) as u32) << 5)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::RiscvPCRelHi20 => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);

            // save for later reference with RiscvPCRelLo12I
            riscv_pcrel_hi20s.insert(reloc_address, reloc_delta as u32);

            let reloc_delta = ((reloc_delta.wrapping_add(0x800) & 0xfffff000) as u32)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::RiscvPCRelLo12I => unsafe {
            let (reloc_address, reloc_abs) = r.for_address(body, target_func_address as u64);
            let reloc_delta = ((riscv_pcrel_hi20s.get(&(reloc_abs as usize)).expect(
                "R_RISCV_PCREL_LO12_I relocation target must be a symbol with R_RISCV_PCREL_HI20",
            ) & 0xfff)
                << 20)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::RiscvCall => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let reloc_delta = ((reloc_delta & 0xfff) << 52)
                | (reloc_delta.wrapping_add(0x800) & 0xfffff000)
                | read_unaligned(reloc_address as *mut u64);
            write_unaligned(reloc_address as *mut u64, reloc_delta);
        },
        RelocationKind::LArchAbsHi20 => unsafe {
            let (reloc_address, reloc_abs) = r.for_address(body, target_func_address as u64);
            let reloc_abs = ((((reloc_abs >> 12) & 0xfffff) as u32) << 5)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_abs);
        },
        RelocationKind::LArchAbsLo12 => unsafe {
            let (reloc_address, reloc_abs) = r.for_address(body, target_func_address as u64);
            let reloc_abs =
                (((reloc_abs & 0xfff) as u32) << 10) | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_abs);
        },
        RelocationKind::LArchAbs64Hi12 => unsafe {
            let (reloc_address, reloc_abs) = r.for_address(body, target_func_address as u64);
            let reloc_abs = ((((reloc_abs >> 52) & 0xfff) as u32) << 10)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_abs);
        },
        RelocationKind::LArchAbs64Lo20 => unsafe {
            let (reloc_address, reloc_abs) = r.for_address(body, target_func_address as u64);
            let reloc_abs = ((((reloc_abs >> 32) & 0xfffff) as u32) << 5)
                | read_unaligned(reloc_address as *mut u32);
            write_unaligned(reloc_address as *mut u32, reloc_abs);
        },
        RelocationKind::Aarch64AdrPrelPgHi21 | RelocationKind::MachoArm64RelocGotLoadPage21 => unsafe {
            let (reloc_address, delta) = r.for_address(body, target_func_address as u64);

            let delta = delta as isize;
            assert!(
                ((-1 << 32)..(1 << 32)).contains(&delta),
                "can't generate page-relative relocation with ±4GB `adrp` instruction"
            );

            let op = read_unaligned(reloc_address as *mut u32);
            let delta = delta >> 12;
            let immlo = ((delta as u32) & 0b11) << 29;
            let immhi = (((delta as u32) >> 2) & 0x7ffff) << 5;
            let mask = !((0x7ffff << 5) | (0b11 << 29));
            let op = (op & mask) | immlo | immhi;

            write_unaligned(reloc_address as *mut u32, op);
        },
        RelocationKind::Aarch64AdrPrelLo21 => unsafe {
            let (reloc_address, delta) = r.for_address(body, target_func_address as u64);

            let delta = delta as isize;
            assert!(
                ((-1 << 20)..(1 << 20)).contains(&delta),
                "can't generate an ADR_PREL_LO21 relocation with an immediate larger than 20 bits"
            );

            let op = read_unaligned(reloc_address as *mut u32);
            let immlo = ((delta as u32) & 0b11) << 29;
            let immhi = (((delta as u32) >> 2) & 0x7ffff) << 5;
            let mask = !((0x7ffff << 5) | (0b11 << 29));
            let op = (op & mask) | immlo | immhi;

            write_unaligned(reloc_address as *mut u32, op);
        },
        RelocationKind::Aarch64AddAbsLo12Nc => unsafe {
            let (reloc_address, delta) = r.for_address(body, target_func_address as u64);

            let delta = delta as isize;
            let op = read_unaligned(reloc_address as *mut u32);
            let imm = ((delta as u32) & 0xfff) << 10;
            let mask = !((0xfff) << 10);
            let op = (op & mask) | imm;

            write_unaligned(reloc_address as *mut u32, op);
        },
        RelocationKind::Aarch64Ldst128AbsLo12Nc => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let reloc_delta = ((reloc_delta as u32 & 0xfff) >> 4) << 10
                | (read_unaligned(reloc_address as *mut u32) & 0xFFC003FF);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::Aarch64Ldst64AbsLo12Nc => unsafe {
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let reloc_delta = ((reloc_delta as u32 & 0xfff) >> 3) << 10
                | (read_unaligned(reloc_address as *mut u32) & 0xFFC003FF);
            write_unaligned(reloc_address as *mut u32, reloc_delta);
        },
        RelocationKind::MachoArm64RelocGotLoadPageoff12 => unsafe {
            // Hacky: we replace the `ldr` instruction with an `add` instruction, as we already
            // know the absolute address of a function, and we don't need any GOT.
            let (reloc_address, reloc_delta) = r.for_address(body, target_func_address as u64);
            let ldr = read_unaligned(reloc_address as *mut u32);
            let xn = ldr & 0xFFFFF;

            let new_op = 0x91000000 | (reloc_delta as u32) << 10 | (xn << 5) | xn;
            write_unaligned(reloc_address as *mut u32, new_op);
        },
        RelocationKind::MachoArm64RelocSubtractor | RelocationKind::MachoX86_64RelocSubtractor => unsafe {
            let (reloc_address, reloc_sub) = r.for_address(body, target_func_address as u64);
            macho_aarch64_subtractor_addresses.insert(reloc_address);
            write_unaligned(reloc_address as *mut u64, reloc_sub);
        },

        RelocationKind::MachoArm64RelocUnsigned | RelocationKind::MachoX86_64RelocUnsigned => unsafe {
            let (reloc_address, mut reloc_delta) = r.for_address(body, target_func_address as u64);

            if macho_aarch64_subtractor_addresses.contains(&reloc_address) {
                reloc_delta -= read_unaligned(reloc_address as *mut u64);
            }

            write_unaligned(reloc_address as *mut u64, reloc_delta);
        },

        RelocationKind::MachoArm64RelocPointerToGot => unsafe {
            if let Some(got) = got_info {
                let base = got.0;
                let base = std::mem::transmute::<usize, *const usize>(base);

                if let Some(reloc_idx) = got.1.map.get(&reloc_target) {
                    let got_address = base.wrapping_add(*reloc_idx);

                    let (reloc_address, _reloc_delta) = r.for_address(body, got_address as u64);

                    write_unaligned(reloc_address as *mut u64, got_address as u64);
                } else {
                    panic!("Missing GOT info for reloc target {reloc_target:?}");
                }
            } else {
                panic!("Missing GOT info for reloc target {reloc_target:?}");
            }
        },
        kind => panic!(
            "Relocation kind unsupported in the current architecture {}",
            kind
        ),
    }
}

/// Links a module, patching the allocated functions with the
/// required relocations and jump tables.
pub fn link_module<'a>(
    _module: &ModuleInfo,
    allocated_functions: &PrimaryMap<LocalFunctionIndex, FunctionExtent>,
    function_relocations: impl Iterator<
        Item = (
            LocalFunctionIndex,
            impl Iterator<Item = &'a (impl RelocationLike + 'a)>,
        ),
    >,
    allocated_sections: &PrimaryMap<SectionIndex, SectionBodyPtr>,
    section_relocations: impl Iterator<
        Item = (
            SectionIndex,
            impl Iterator<Item = &'a (impl RelocationLike + 'a)>,
        ),
    >,
    libcall_trampolines: SectionIndex,
    trampoline_len: usize,
    got_info: Option<(usize, GOT)>,
) {
    let mut riscv_pcrel_hi20s: HashMap<usize, u32> = HashMap::new();

    for (i, section_relocs) in section_relocations {
        let body = *allocated_sections[i] as usize;
        for r in section_relocs {
            apply_relocation(
                body,
                r,
                allocated_functions,
                allocated_sections,
                libcall_trampolines,
                trampoline_len,
                &mut riscv_pcrel_hi20s,
                &got_info,
            );
        }
    }
    for (i, function_relocs) in function_relocations {
        let body = *allocated_functions[i].ptr as usize;
        for r in function_relocs {
            apply_relocation(
                body,
                r,
                allocated_functions,
                allocated_sections,
                libcall_trampolines,
                trampoline_len,
                &mut riscv_pcrel_hi20s,
                &got_info,
            );
        }
    }
}
