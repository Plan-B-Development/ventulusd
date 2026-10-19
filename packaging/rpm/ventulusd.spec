# Packs the binaries packaging/linux/build.sh built on Debian 13 (DEC-499);
# nothing is compiled here. build.sh defines the version, the release, the
# staged tree (packaging/linux/stage.sh) and the %files list generated from it.
%global debug_package %{nil}
# Already stripped, so the .rpm carries the same bytes as the .deb and the tarball.
%global __strip /bin/true

Name:           ventulusd
Version:        %{ventulus_version}
Release:        %{ventulus_release}
Summary:        Hardware fan control daemon for Linux (OpenFan, hwmon, GPU)
License:        MIT
URL:            https://github.com/Plan-B-Development/ventulusd
ExclusiveArch:  x86_64
BuildRequires:  systemd-rpm-macros
Requires:       kmod
# As the .deb's Suggests: never installed by default, so DKMS and kernel headers
# stay the user's choice (packaging_linux.rs pins it).
Suggests:       lm_sensors, dkms
%{?systemd_requires}

%description
ventulusd controls PC fans through an OpenFAN controller, motherboard fan
headers (hwmon) and AMD GPU firmware fan curves. The Ventulus GUI and the
ventulus-tray panel client talk to it over a local socket.

It gives every fan header back exactly as it found it whenever it stops, and
forces full speed on a thermal emergency whatever profile is active.

%prep

%build

%install
cp -a %{ventulus_stage}/. %{buildroot}/

# The post-install scriptlet applies the presets, and Fedora's
# 99-default-disable.preset ("disable *") leaves the unit disabled on install.
# An upgrade restarts it only if it is running, and removal stops it first:
# every stop is a systemd stop, so ExecStopPost gives the fans back on each
# path (DEC-382). No macro may be named in these comments: rpm expands them.
%post
%systemd_post ventulusd.service

%preun
%systemd_preun ventulusd.service

%postun
%systemd_postun_with_restart ventulusd.service

%files -f %{ventulus_filelist}
