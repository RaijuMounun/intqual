# Maintainer: Eren Keskinoğlu <erenkeskinoglu@outlook.com>
pkgname=intqual
pkgver=1.0.0
pkgrel=1
pkgdesc="A dual-layer unprivileged network latency analyzer and observability tool (ICMP/TCP)"
arch=('x86_64' 'aarch64')
url="https://github.com/RaijuMounun/intqual"
license=('MIT')
depends=('gcc-libs')
makedepends=('cargo')
source=("${pkgname}-${pkgver}.tar.gz::https://github.com/RaijuMounun/${pkgname}/archive/refs/tags/${pkgver}.tar.gz")
sha256sums=('f8c0de10f2e83e77093de45ecb31fc584af37f6bbaf2a6710df94d8767fc85d6')

build() {
  cd "${pkgname}-${pkgver}"
  cargo build --release --locked
}

package() {
  cd "${pkgname}-${pkgver}"
  install -Dm755 "target/release/${pkgname}" "${pkgdir}/usr/bin/${pkgname}"
  install -Dm644 "LICENSE" "${pkgdir}/usr/share/licenses/${pkgname}/LICENSE"
}