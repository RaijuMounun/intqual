# Maintainer: Eren Keskinoğlu <erenkeskinoglu@outlook.com>
pkgname=intqual
pkgver=1.0.7
pkgrel=1
pkgdesc="A dual-layer unprivileged network latency analyzer and observability tool (ICMP/TCP)"
arch=('x86_64' 'aarch64')
url="https://github.com/RaijuMounun/intqual"
license=('MIT')
depends=('gcc-libs')
makedepends=('cargo' 'cmake' 'go')
options=('!lto')
source=("${pkgname}-${pkgver}.tar.gz::https://github.com/RaijuMounun/${pkgname}/archive/refs/tags/v${pkgver}.tar.gz")
sha256sums=('b694fa001ecb19e627df4a387c00709e4029cc9d5619386d892a723a218c00a8')

build() {
  cd "${pkgname}-${pkgver}"
  cargo build --release --locked
}

package() {
  cd "${pkgname}-${pkgver}"
  install -Dm755 "target/release/${pkgname}" "${pkgdir}/usr/bin/${pkgname}"
  install -Dm644 "LICENSE" "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}
