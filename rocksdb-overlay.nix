final: prev: {

  rocksdb = prev.rocksdb.overrideAttrs (oldAttrs: rec {
    version = "8.1.1";

    src = final.fetchFromGitHub {
      owner = "facebook";
      repo = oldAttrs.pname;
      rev = "v${version}";
      hash = "sha256-79hRtc5QSWLLyjRGCmuYZSoIc9IcIsnl8UCinz2sVw4=";
    };
  });
}
