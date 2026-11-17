# Debian 13 for trying the Linux build: CA certificates, the Liberation
# fonts (Arial / Georgia metrics), a Chinese font, and X11 libraries with a
# virtual display (Xvfb) for the windowed browser.
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
    ca-certificates fonts-liberation fonts-wqy-microhei \
    libx11-6 libx11-xcb1 libxcursor1 libxrandr2 libxi6 libxkbcommon-x11-0 \
    xvfb x11-apps netpbm \
 && rm -rf /var/lib/apt/lists/*
ENV LANG=C.UTF-8
