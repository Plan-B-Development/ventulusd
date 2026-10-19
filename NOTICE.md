# Third-Party License Notices

`ventulusd` is distributed under the **MIT License** (see the package
metadata in `daemon/Cargo.toml`). The `ventulus-tray` binary shipped in the
same package is likewise MIT (`tray/Cargo.toml`). Both depend on third-party
crates under their own licenses, including two whose license differs from MIT
and which are called out here for transparency:

- **serialport** (4.9.0): **Mozilla Public License 2.0 (MPL-2.0)**.
  <https://github.com/serialport/serialport-rs>
- **ksni** (0.3.6): **The Unlicense**, used by `ventulus-tray` for the
  StatusNotifierItem / dbusmenu implementation (DEC-352).
  <https://github.com/iovxw/ksni>

MPL-2.0 is weak, file-level copyleft: it permits use within an MIT-licensed
binary and does not change this project's own license. The obligation to make
the MPL-covered source available on request applies to the `serialport` crate's
own files and is satisfied by that project's public upstream repository and its
crates.io publication.

The Unlicense is a public-domain dedication, not a copyleft licence. It imposes
no condition of any kind (no re-linking obligation, no source-availability
obligation, not even attribution), so it places no requirement on this project
or on anyone redistributing it. It is recorded here only because it is not the
MIT licence the rest of the tree carries. It is OSI-approved and listed by the
FSF as Free/Libre.

## Board sensor facts from the it87 project (DEC-464, DEC-500)

`daemon/src/hwmon/board_catalogue_data.rs` is generated from the Gigabyte sensor
configurations in **frankcrawford/it87** (`Sensors configs/Gigabyte/configs/`:
`gigabyte-it87-amd.conf`, `gigabyte-it87-intel.conf`, and the channels on which
`gigabyte-it87-intel-kabylakex.conf` and `gigabyte-it87-intel-skylakex.conf` agree),
which are distributed under the **GNU General Public License v2.0**.
<https://github.com/frankcrawford/it87>

What the daemon carries is the factual content of their `label`, `compute` and
`ignore` lines for voltage inputs (`inN`), fan tachometers (`fanN`), PWM outputs
(`pwmN`) and temperatures (`tempN`): which board rail, fan header or temperature
each Super-I/O channel is wired to, the divider ratio on an input, and which
channels a board's configuration does not map, plus the board names each SIV's
comment block lists. It is re-expressed as a Rust table keyed the way those
configurations key it (chip and Gigabyte SIV). No configuration file is copied or
shipped. The generated file's header records the upstream commit it was produced
from, and which channels were left out because an independent derivation
disagrees with the configuration. The same approach was already taken for the
catalogue-derived fan-header labels in `ventulus` (DEC-421). We thank the it87
maintainers and contributors for publishing this per-board data.

## The flash-id helper from picotool (DEC-483)

To check that an RP2040 in its bootloader is the OpenFAN board an update was
started for, the daemon reads the flash chip's unique id the way picotool does:
it loads a 152-byte Thumb routine into the RP2040's XIP SRAM and has the boot ROM
run it. That routine is `picoboot_flash_id/flash_id.bin` from **picotool** 2.3.1
(<https://github.com/raspberrypi/picotool>), carried byte for byte as
`FLASH_ID_HELPER` in `daemon/src/serial/picoboot.rs` and distributed under the
**BSD 3-Clause License**:

> Copyright 2020 (c) 2020 Raspberry Pi (Trading) Ltd.
>
> Redistribution and use in source and binary forms, with or without
> modification, are permitted provided that the following conditions are met:
>
> 1. Redistributions of source code must retain the above copyright notice, this
>    list of conditions and the following disclaimer.
>
> 2. Redistributions in binary form must reproduce the above copyright notice,
>    this list of conditions and the following disclaimer in the documentation
>    and/or other materials provided with the distribution.
>
> 3. Neither the name of the copyright holder nor the names of its contributors
>    may be used to endorse or promote products derived from this software
>    without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
> ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
> WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
> DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
> ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
> (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
> LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
> ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
> (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
> SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

The PICOBOOT client around it is the daemon's own, written from the RP2040
datasheet (§2.8.5).

The full dependency licence set can be regenerated with `cargo tree` /
`cargo about`; this notice records only the non-permissive case (audit P2-H,
DEC-155).
