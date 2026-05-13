# Ubuntu packaging

Build a local binary package from the repository root:

```sh
./releng/ubuntu/build-deb.sh
```

The package is written to `target/ubuntu/`.

To build inside the Ubuntu container used for release testing:

```sh
./releng/ubuntu/build-deb-container.sh
```

Set `DEB_BUILD_IMAGE` to override the container image. The default is
`ubuntu:26.04`.

Build requirements:

- `cargo`
- `dpkg-deb`
- GTK 4 development files
- OpenSSL development files

Runtime dependencies are declared in `control.in`.
