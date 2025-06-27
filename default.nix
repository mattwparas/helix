{
  lib,
  rustPlatform,
  callPackage,
  runCommand,
  installShellFiles,
  git,
  gitRev ? null,
  grammarOverlays ? [],
  includeGrammarIf ? _: true,
  includeCogs ? true,
}: let
  fs = lib.fileset;

  src = fs.difference (fs.gitTracked ./.) (fs.unions [
    ./.envrc
    ./rustfmt.toml
    ./screenshot.png
    ./book
    ./docs
    ./runtime
    ./flake.lock
    (fs.fileFilter (file: lib.strings.hasInfix ".git" file.name) ./.)
    (fs.fileFilter (file: file.hasExt "svg") ./.)
    (fs.fileFilter (file: file.hasExt "md") ./.)
    (fs.fileFilter (file: file.hasExt "nix") ./.)
  ]);

  # Next we actually need to build the grammars and the runtime directory
  # that they reside in. It is built by calling the derivation in the
  # grammars.nix file, then taking the runtime directory in the git repo
  # and hooking symlinks up to it.
  grammars = callPackage ./grammars.nix {inherit grammarOverlays includeGrammarIf;};
  runtimeDir = runCommand "helix-runtime" {} ''
    mkdir -p $out
    ln -s ${./runtime}/* $out
    rm -r $out/grammars
    ln -s ${grammars} $out/grammars
  '';
in
  rustPlatform.buildRustPackage (self: {
    cargoLock = {
      lockFile = ./Cargo.lock;
      # This is not allowed in nixpkgs but is very convenient here: it allows us to
      # avoid specifying `outputHashes` here for any git dependencies we might take
      # on temporarily.
      allowBuiltinFetchGit = true;
    };

    nativeBuildInputs = [
      installShellFiles
      git
    ];

    buildType = "release";

    name = with builtins; (fromTOML (readFile ./helix-term/Cargo.toml)).package.name;
    src = fs.toSource {
      root = ./.;
      fileset = src;
    };

    # Helix attempts to reach out to the network and get the grammars. Nix doesn't allow this.
    HELIX_DISABLE_AUTO_GRAMMAR_BUILD = "1";

    # So Helix knows what rev it is.
    HELIX_NIX_BUILD_REV = gitRev;

    doCheck = false;
    strictDeps = true;

    # Sets the Helix runtime dir to the grammars
    env.HELIX_DEFAULT_RUNTIME = "${runtimeDir}";


    preBuild = lib.optionalString includeCogs ''
      echo "IN PRE BUILD"
      export STEEL_HOME=$PWD/steel_home  # code-gen will write files relative to $STEEL_HOME
      export STEEL_LSP_HOME=$PWD/lsp_home  # required to generate primitives for language server
      mkdir -p $STEEL_LSP_HOME
      mkdir -p $STEEL_HOME
      cargoBuildLog=$(mktemp cargoBuildLogXXXX.json)
      cargo run --package xtask -- code-gen --message-format json-render-diagnostics >"$cargoBuildLog"
    '';

    # Get all the application stuff in the output directory.
    postInstall = ''
      mkdir -p $out/lib
      installShellCompletion ${./contrib/completion}/hx.{bash,fish,zsh}
      mkdir -p $out/share/{applications,icons/hicolor/{256x256,scalable}/apps}
      cp ${./contrib/Helix.desktop} $out/share/applications/Helix.desktop
      cp ${./logo.svg} $out/share/icons/hicolor/scalable/apps/helix.svg
      cp ${./contrib/helix.png} $out/share/icons/hicolor/256x256/apps/helix.png
    '' + lib.optionalString includeCogs ''
      mkdir -p $out/steel/cogs $out/steel/steel-language-server
      cp -r steel_home/cogs/* "$out/steel/cogs"
      cp -r lsp_home/* "$out/steel/steel-language-server"
    '';

    meta.mainProgram = "hx";

  })
