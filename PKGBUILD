# Maintainer: Eren Keskinoğlu <erenkeskinoglu@outlook.com>
pkgname=intqual
pkgver=1.3.0
pkgrel=1
pkgdesc="A dual-layer unprivileged network latency analyzer and observability tool (ICMP/TCP)"
arch=('x86_64' 'aarch64')
url="https://github.com/RaijuMounun/intqual"
license=('MIT')
depends=('gcc-libs')
makedepends=('cargo' 'cmake')
options=('!lto')
source=("${pkgname}-${pkgver}.tar.gz::https://github.com/RaijuMounun/${pkgname}/archive/refs/tags/v${pkgver}.tar.gz")
sha256sums=('461bb2296985dae8237002af47eb376c567da56942c2dc209d70603fe1335e52')

build() {
  cd "${pkgname}-${pkgver}"
  cargo build --release --locked
}

package() {
  cd "${pkgname}-${pkgver}"
  install -Dm755 "target/release/${pkgname}" "${pkgdir}/usr/bin/${pkgname}"
  install -Dm644 "LICENSE" "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}
