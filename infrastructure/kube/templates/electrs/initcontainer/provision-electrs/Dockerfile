FROM node:13-alpine AS install

RUN apk --no-cache add git

WORKDIR /tmp

COPY ./package.json /tmp/package.json
COPY ./package-lock.json /tmp/package-lock.json

RUN npm ci

FROM node:13-alpine

WORKDIR /tmp

COPY --from=install /tmp .

COPY ./electrs-config.toml /tmp/electrs-config.toml

COPY ./provision-electrs.js /tmp/provision-electrs.js

RUN mkdir -p /mnt/electrs

ENTRYPOINT ["node", "./provision-electrs.js"]
