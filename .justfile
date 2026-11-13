# Build the release tarballs and their SHA256SUMS into dist/
release:
    nix --extra-experimental-features 'nix-command flakes' build .#release
    rm -rf dist && mkdir dist && cp result/* dist/ && chmod u+w dist/*

# Build the release, then sign its SHA256SUMS into dist/SHA256SUMS.asc
sign: release
    gpg --detach-sign --armor dist/SHA256SUMS
