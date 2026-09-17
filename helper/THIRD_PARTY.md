# Static helper dependencies

The static payloads use musl libc 1.2.5, downloaded from
https://musl.libc.org/releases/musl-1.2.5.tar.gz.
Source SHA-256: `a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4`.
Its full copyright notice is in [MUSL-COPYRIGHT](MUSL-COPYRIGHT).

GCC startup/integer runtime objects may also be linked. GCC's runtime library
exception permits these combinations; see
https://www.gnu.org/licenses/gcc-exception-3.1.html and
https://gcc.gnu.org/git/?p=gcc.git;a=blob;f=COPYING.RUNTIME.
The build manifest records the compiler version used for each payload.
