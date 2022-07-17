{ lib
, stdenv
, fetchFromGitHub
, testers
, bison
, cadical
, cmake
, flex
, perl
}:

stdenv.mkDerivation rec {
  pname = "cbmc";
  version = "5.61.0";

  src = fetchFromGitHub {
    owner = "diffblue";
    repo = pname;
    rev = "${pname}-${version}";
    sha256 = "sha256-IIGlKTsb6M8Xm34n6WlZFd3RsaQ9pHymj2xp1HFSHBc=";
  };

  nativeBuildInputs = [
    bison
    cmake
    flex
    perl
  ];

  buildInputs = [ cadical ];

  # do not download sources
  patches = [
    ./0001-Do-not-download-sources-in-cmake.patch
  ];

  # TODO: add jbmc support
  cmakeFlags = [ "-DWITH_JBMC=OFF" "-Dsat_impl=cadical" "-Dcadical_INCLUDE_DIR=${cadical.dev}/include" ];

  preConfigure = ''
    # fix library_check.sh interpreter error
    patchShebangs .
  '';

  meta = with lib; {
    description = "CBMC is a Bounded Model Checker for C and C++ programs";
    homepage = "http://www.cprover.org/cbmc/";
    license = licenses.bsdOriginal;
    maintainers = with maintainers; [ jiegec ];
    platforms = platforms.linux;
  };
}
