# help I need somebody
_help:
    just -l

# Run all tests using nextest.
test:
    cargo nextest run --all-targets --future-incompat-report

# Run the same checks we run in CI. Requires nightly.
@ci: test
    cargo clippy --all-targets -- -D warnings
    cargo audit
    cargo +nightly fmt --check --all

# format using nightly
fmt:
    cargo +nightly fmt

# Ask for clippy's opinion.
lint: fmt
    cargo clippy --all-targets --fix

# Install required tools
setup:
    #!/usr/bin/env bash
    if [[ ! command rustc ]]; then
    	printf "Installing 🦀 {{ BOLD_RED }}Rust{{ RESET }}…\n"
    	curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    else
    	rustup update
    fi
    if [[ ! command brew ]]; then
    	printf "Installing 🍺 {{ BOLD_YELLOW }}Homebrew{{ RESET }}…\n"
    	/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    else
    	brew upgrade
    fi
    brew tap ceejbot/tap
    brew install fzf cargo-nextest tomato semver-bump graphviz
    rustup install nightly -c rustfmt

# Tag a new version for release.
version BUMP:
    #!/usr/bin/env bash
    set -e
    current=$(tomato get package.version Cargo.toml)
    version=$(semver-bump {{ BUMP }} "$current")
    tomato set package.version "$version" Cargo.toml &> /dev/null
    cargo generate-lockfile
    git commit Cargo.toml Cargo.lock -m "v${version}"
    git tag "v${version}"
    echo "Release tagged for version v${version}"

RESET := "\\e[0m"
BOLD := "\\e[1m"
BOLD_RED := "\\e[1;31m"
BOLD_GREEN := "\\e[1;32m"
BOLD_YELLOW := "\\e[1;33m"
BOLD_BLUE := "\\e[1;34m"
BOLD_MAGENTA := "\\e[1;35m"
BOLD_CYAN := "\\e[1;36m"
BLACK := "\\e[30m"
BLACK_BG := "\\e[40m"
RED := "\\e[31m"
RED_BG := "\\e[41m"
GREEN := "\\e[32m"
GREEN_BG := "\\e[42m"
YELLOW := "\\e[33m"
YELLOW_BG := "\\e[43"
BLUE := "\\e[34m"
BLUE_BG := "\\e[44m"
MAGENTA := "\\e[35m"
MAGENTA_BG := "\\e[45m"
CYAN := "\\e[36m"
CYAN_BG := "\\e[46m"
WHITE := "\\e[37m"
WHITE_BG := "\\e[47m"
DEFAULT := "\\e[39m"
DEFAULT_BG := "\\e[49m"
