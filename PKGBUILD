# Maintainer: LyricTTY contributors
pkgname=lyrictty-git
pkgver=0.1.0.r1
pkgrel=1
pkgdesc='Synchronized lyrics and adaptive music UI for the terminal (development)'
arch=('x86_64' 'aarch64')
url='https://github.com/6axam/LyricTTY'
license=('MIT')
depends=('gcc-libs' 'glibc')
makedepends=('cargo' 'git')
optdepends=('mpv: native playback' 'yt-dlp: public YouTube and YouTube Music URLs')
provides=('lyrictty')
conflicts=('lyrictty')
source=('git+https://github.com/6axam/LyricTTY.git')
sha256sums=('SKIP')

pkgver() {
  cd LyricTTY
  printf '0.1.0.r%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}
prepare() {
  cd LyricTTY
  cargo fetch --locked
}
build() {
  cd LyricTTY
  cargo build --release --frozen
}
package() {
  cd LyricTTY
  install -Dm755 target/release/lyrictty "$pkgdir/usr/bin/lyrictty"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
}
