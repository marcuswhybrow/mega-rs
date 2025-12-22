{
  description = "Unoffical MEGA API client for Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    naersk.url = "github:nix-community/naersk";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs: let 
    pkgs = import inputs.nixpkgs { 
      system = "x86_64-linux"; 
    };

    toolchain = inputs.fenix.packages.x86_64-linux.combine [
      inputs.fenix.packages.x86_64-linux.latest.toolchain
      inputs.fenix.packages.x86_64-linux.targets.wasm32-unknown-unknown.latest.rust-std
    ];

    naersk = pkgs.callPackage inputs.naersk {
      cargo = toolchain;
      rustc = toolchain;
    };

    cargo = (pkgs.lib.importTOML ./Cargo.toml).package;
  in {
    packages.x86_64-linux.mega-rs = naersk.buildPackage {
      name = cargo.name;
      version = cargo.version;
      src = pkgs.lib.cleanSource ./.;
    };
    packages.x86_64-linux.default = inputs.self.packages.x86_64-linux.mega-rs;

    devShells.x86_64-linux.default = pkgs.mkShell {
      RUST_BACKTRACE = "full";
      nativeBuildInputs = [ 
        toolchain
        pkgs.bacon 
        pkgs.rust-analyzer-unwrapped
      ];
    };

  };
}
