# Carbone Community Edition as an HTTP service for Railway (or any container host).
#
# Carbone converts documents through LibreOffice (soffice) and Python. On Linux
# carbone only looks for `soffice.bin` in PATH or in /opt/libreoffice*/program,
# while Debian/Ubuntu packages install it under /usr/lib/libreoffice/program,
# hence the symlink below.
FROM node:24-bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      libreoffice-writer \
      libreoffice-calc \
      libreoffice-impress \
      python3 \
      fonts-dejavu-core \
      fonts-liberation \
 && ln -sf /usr/lib/libreoffice/program/soffice.bin /usr/local/bin/soffice.bin \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci --omit=dev && npm cache clean --force

COPY . .

ENV NODE_ENV=production
EXPOSE 3000

# Templates and rendered files live in the temp dir, writable by the node user.
USER node

CMD ["node", "server.js"]
