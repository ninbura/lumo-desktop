# Toolchain image for the bridge E2E suite.
#
# WebdriverIO connects to the app's embedded WebDriver server. WebKitGTK remains
# the real rendering engine; no external tauri-driver or WebKitWebDriver is needed.
# Deliberately toolchain-only: the app is compiled at run time from the mounted
# working tree (see run.sh), so the image stays reusable across code changes and the
# cargo cache lives in a volume rather than a layer.
# 22.04 because build-linux ships from this image: the binary needs this glibc or newer.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

# libwebkit2gtk-4.1-dev is the engine Tauri v2 links against; libdbus-1-dev is what the keyring
# crate links; libxdo/appindicator/rsvg by Tauri itself.
#
# No Secret Service is installed on purpose: creating its default collection needs the
# graphical gcr-prompter, which can't be answered headlessly. Tests run against the
# no-secure-storage state instead.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        dbus \
        dbus-x11 \
        file \
        git \
        libayatana-appindicator3-dev \
        libdbus-1-dev \
        librsvg2-dev \
        libssl-dev \
        libwebkit2gtk-4.1-dev \
        libxdo-dev \
        pkg-config \
        xauth \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

# Ubuntu's own Node is below WebdriverIO 9's floor (^18.20 || ^20.13 || >=22).
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable

WORKDIR /work

CMD ["bash"]
