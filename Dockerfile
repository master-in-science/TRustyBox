FROM alpine:3.16.2

WORKDIR /opt/

RUN apk add curl protoc musl-dev gzip git tmux

RUN chown -R root:root /opt/
