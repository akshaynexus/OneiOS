#!/usr/bin/env python3
"""Post-link fixes of the hosted kernel's text: kernel/hosted-fixup.py KERNEL (rewritten in place).

Run by kernel/build-hosted.sh on the linked kernel, before it is sealed again (the kernel's text is covered by
corecrypto's integrity HMAC, so its instructions cannot be changed at boot).

  mrs xN, MPIDR_EL1  ->  ldr wN, [x18]
      Under xnuhost a vCPU learns who it is from its struct xh_cpu (x18 in kernel code; offset 0 is the vCPU
      index, which is the "reg" of its /cpus node). MPIDR_EL1 cannot say it: at EL0 Linux answers that
      register itself with the same value on every core. XNU's own reads and ArmCpu's (it parks a secondary
      core whose MPIDR is not the affinity it was started with) all become the vCPU index.

Only the executable segments of an MH_EXECUTE arm64/arm64e kernel are touched; anything else is refused.
"""
import struct
import sys

MRS_MPIDR = 0xd53800a0          # mrs xN, MPIDR_EL1 (S3_0_C0_C0_5)
LDR_INDEX = 0xb9400240          # ldr wN, [x18]


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    path = sys.argv[1]
    data = bytearray(open(path, 'rb').read())
    magic, cpu, _, kind, ncmds = struct.unpack_from('<5I', data)
    if (magic, cpu, kind) != (0xfeedfacf, 0x100000c, 2):
        raise SystemExit('hosted-fixup: not an arm64 MH_EXECUTE kernel')
    offset, count, sites = 32, 0, []
    for _ in range(ncmds):
        cmd, length = struct.unpack_from('<II', data, offset)
        if cmd == 0x19:
            name, va, _, fileoff, filesize, _, initprot = struct.unpack_from('<16s4QII', data, offset + 8)
            if initprot & 4:
                for at in range(fileoff, fileoff + (filesize & ~3), 4):
                    insn = struct.unpack_from('<I', data, at)[0]
                    if insn & 0xffffffe0 == MRS_MPIDR:
                        struct.pack_into('<I', data, at, LDR_INDEX | (insn & 31))
                        sites.append(va + at - fileoff)
                        count += 1
        offset += length
    open(path, 'wb').write(data)
    print(f'hosted-fixup: {count} MPIDR_EL1 reads now read the vCPU index' + (': ' + ' '.join(f'{s:#x}' for s in sites[:16]) if sites else ''))


main()
