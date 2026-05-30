# cade

an intelligent, cascading environment manager. similar to [direnv](https://direnv.net/) with composable layers that stack as you navigate nested project directories and a strong nix focus.

`cade` walks up the directory tree from your current directory, collecting `.cade` configuration files and merging their environments in parent-first order. this lets nested projects inherit and extend parent configurations.

## features

- **layered environments**: `.cade` files compose from parent to child directories
- **multiple sources**: load environment variables from nix flakes, nix shell files, `.env` files, or arbitrary commands
- **multi-shell support**: bash, zsh, fish, nushell, elvish, and murex
- **lifecycle hooks**: run commands before/after environment load and unload
- **environment purification**: optionally discard the ambient environment for a layer
- **permission system**: directories must be explicitly allowed before activation
- **safe by construction**: values are shell-quoted, so secrets or `$(...)` in a `.env` are never executed
- **direnv compatibility**: reads the common declarative subset of `.envrc` files (`use flake`, `dotenv`, …) without executing them

## installation

### with nix (flakes)

add cade as an input and use the module for your platform. the module installs
cade and wires its hook into your interactive shells for bash/zsh/fish - otherwise see lib.snippets in the flake
or programs.cade.shellSnippets in the nixos module.

```nix
{
  inputs.cade.url = "github:manic-systems/cade";

  # NixOS
  outputs = { self, nixpkgs, cade, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        cade.nixosModules.default
        { programs.cade.enable = true; }
      ];
    };
  };
}
```

```nix
# nix-darwin, identical options
darwinConfigurations.mymac = darwin.lib.darwinSystem {
  modules = [
    cade.darwinModules.default
    { programs.cade.enable = true; }
  ];
};
```

the module wires the hook into interactive **bash**, **zsh**, and **fish**
(the shells these systems integrate centrally). toggle per shell:

```nix
programs.cade = {
  enable = true;
  enableBashIntegration = true;  # default
  enableZshIntegration  = true;  # default; needs programs.zsh.enable
  enableFishIntegration = true;  # default; needs programs.fish.enable
  verbosity = "normal";          # null, quiet, normal, vars, trace
  longRunningWarningMs = 5000;   # null, or a positive integer
  shellGcRootTtlSeconds = 2592000; # null, or a positive integer
};
```

set `verbosity` or `longRunningWarningMs` unless you provide
`programs.cade.configFile`.

for **nushell**, **elvish**, or **murex**, add the hook to your user shell config; see
[Manual setup](#manual-setup) below. the ready-made init lines are also exposed
as `config.programs.cade.shellSnippets.<shell>` (with the module) and as
`cade.lib.shellSnippets.<shell>`.

### with cargo

```
cargo install --path .
```

## manual setup

if you're not using the nixos module or bash/zsh/fish, add the hook
to your shell's startup:

**bash** (`~/.bashrc`):
```bash
eval "$(cade hook bash)"
```

**zsh** (`~/.zshrc`):
```zsh
eval "$(cade hook zsh)"
```

**fish** (`~/.config/fish/config.fish`):
```fish
cade hook fish | source
```

**nushell** (`config.nu`):
```nu
cade hook nushell | save -f ~/.cache/cade/hook.nu
source ~/.cache/cade/hook.nu
```

**elvish** (`~/.elvish/rc.elv`):
```elvish
eval (cade hook elvish | slurp)
```

**murex** (`~/.murex_profile`):
```murex
cade hook murex -> source
```

for nix users with declarative configs, you may want to dump the hook at build time and source it. 
nushell example:

```nix
source ${
  (pkgs.runCommand "cade.nu" { } ''${lib.getExe (getFlakePkg inputs.cade)} hook nushell >> "$out"'')
}
````

## usage

### commands

```
cade allow                    # allow cade in the current .cade directory
cade disallow                 # block cade in the current .cade directory
cade edit                     # open .cade in $EDITOR + allow this path
cade status                   # show activation state, layer chain, permissions
cade hook <SHELL>             # print the shell hook initialization code
cade enter --shell <SHELL>    # activate the environment (used by the hook)
cade exit --shell <SHELL>     # deactivate and restore the previous environment
cade reload --shell <SHELL>   # re-evaluate on directory change (called by the hook)
cade --config /nix/store/cade.toml status  # strict config file override
cade --verbosity vars status  # quiet | normal | vars | trace
```

cade reads an optional TOML config from `$XDG_CONFIG_HOME/cade/config.toml`
(usually `~/.config/cade/config.toml`). `--config <file>` is strict: that exact
file is read instead of the XDG default, and a missing or invalid file is an
error. this is intended for wrappers and declarative systems.

```toml
verbosity = "normal"           # quiet | normal | vars | trace
long_running_warning_ms = 5000
shell_gc_root_ttl_seconds = 2592000
```

CLI flags override environment variables, which override the config file.

verbosity controls cade's human-facing diagnostics:

- `quiet`: no lifecycle text or warnings
- `normal`: load/unload/reload text and warnings (default)
- `vars`: normal plus variable names set/cleared/restored
- `trace`: vars plus layer/cache, external command, and hook details

set `CADE_VERBOSITY` to apply a level to shell hooks without a config file.

external loaders (`load flake`, `load shell`, and `call`) print a warning if
they run for more than 5 seconds. set `long_running_warning_ms` in the config
or `CADE_LONG_RUNNING_WARNING_MS` in the environment to adjust that threshold.

shell roots stay alive while a session is active, then are pruned after 30
days by default. nix-derived loaders (`load flake`, `load shell`, and matching
`.envrc` directives) root the full dev environment closure with a session-scoped
nix profile. non-nix sources fall back to rooting nix store paths that appear in
their environment values. set `shell_gc_root_ttl_seconds` in the config or
`CADE_SHELL_GC_ROOT_TTL_SECONDS` in the environment to adjust the retention
window for inactive roots.
## permissions

cade only composes layers from directories you've **explicitly allowed**.
there is no implicit trust of ancestors or descendants.

- **`cade allow`** approves the current directory and **gap-fills up to your
  nearest already-approved ancestor** (the base), stopping there.
- **activation** composes the contiguous run of *allowed* config directories
  from your current location upward, stopping at the first unapproved or empty ancestor.
  an untrusted `.cade` above your base is never loaded,
  and a malicious `.cade` dropped above your project can't auto-run.
- typical flow: `cade allow` at your project root (the base), then `cade allow`
  at a deep sub-project. the layers in between are approved automatically, but
  nothing above the root.
- **`cade edit`** opens `./.cade` in `$EDITOR` and allows the current directory
  afterwards
- **`cade status`** provides detailed information on the current cade state

## `.cade` file format

one directive per line, `#` are comments

```
# discard the ambient (pre-existing) environment for this layer, while still
# keeping variables inherited from parent .cade layers
pure

# load from flake (default shell or named installable)
load
load flake
load flake devShells.default

# load from shell.nix
load shell
load shell custom-shell.nix

# load from .env file
load env
load env .env.development

# load a direnv .envrc (declarative subset only)
load envrc
load envrc .envrc.local

# run a command and parse its KEY=VALUE output as environment
call python scripts/get-env.py

# set a variable inline. the key must be ALL_CAPS so it can't be mistaken
# for a directive; := for a hard replace
SOMEVAR=somevalue

# inject hooks into cade's lifecycle. these are directly run commands
hook preload echo "loading..."
hook load echo "ready"
hook preunload echo "unloading..."
hook unload echo "done"

# unset specific variables (also drops them from inherited layers)
clear PYTHONPATH NODE_PATH

# reload this layer when extra files change. useful with `call`
watch scripts/get-env.py config/secrets.age

# treat extra variables as colon-lists that accumulate (like PATH)
# propagates through layers
concat PYTHONPATH GOPATH
```

in `.cade`, `.env` files, and `call` output, `KEY=value` follows the variable's
normal mode, while `KEY:=value` forces a **hard replace** that ignores ambient
and parent layers.

## direnv compatibility

cade can read [direnv](https://direnv.net/) `.envrc` files, but does **not**
execute them as shell scripts. it recognizes the declarative subset of
the direnv stdlib that maps cleanly onto cade's own loaders:

| `.envrc`                         | cade equivalent              |
| -------------------------------- | ---------------------------- |
| `use flake` / `use flake .#out`  | `load flake`                 |
| `use nix [file]`                 | `load shell`                 |
| `dotenv` / `dotenv_if_exists`    | `load env`                   |
| `export KEY=value` (literal)     | sets the variable            |
| `PATH_add dir`                   | prepends `dir` to `PATH`     |
| `watch_file f`                   | reloads when `f` changes     |

an `.envrc` is picked up two ways:

- **automatically**: a directory with an `.envrc` but no `.cade` is treated as
  if it contained `load envrc`
- **explicitly**: `load envrc [file]` in a `.cade` composes it as one layer
  alongside other directives 

anything cade can't faithfully reproduce (shell expansion, conditionals,
`layout`, `source_up`, functions, unknown flags) is skipped with a warning.

## example

given this directory structure:

```
~/work/.cade          # load env .env
~/work/project/.cade  # load flake
```

when you `cd ~/work/project`, cade loads the `.env` from `~/work` first, then
layers the flake environment from `~/work/project` on top.

activation also works from a subdirectory that has no `.cade` of its own: cade
walks up to the nearest `.cade` ancestor and activates from there.

## variable composition

cade composes a variable across the ambient environment and each layer, with two
behaviors:

- **concat (list-like vars).** `PATH` and other path-like vars
  (`LD_LIBRARY_PATH`, `*_PATH`, `MANPATH`, `XDG_*_DIRS`, …) **accumulate** rather
  than overwrite. values are ordered **child : parent : … : ambient**: the
  innermost layer comes first and wins, and your existing (ambient) value is
  kept at the end, so system tools stay reachable. mark additional variables
  list-like with `concat VAR` (applies to that layer and inward).
- **replace (everything else).** scalars like `EDITOR` or `CC` are replaced; the
  innermost layer wins and the ambient value is dropped.

two escape hatches:

- `KEY:=value` (in `.env`/`call` output) forces a **hard replace** even for a
  path-like var, with no ambient and no parent layers.
- `pure` discards the ambient environment for that layer, so concat vars
  resolve to the layer stack only (inherited *layer* values are still kept).
  shell runtime variables such as `PWD`, `HOME`, and command status are
  preserved so the interactive shell can still run its hooks.

## how it works

1. the shell hook detects directory changes and calls `cade reload`
2. cade walks up from the current directory to the nearest config directory
   (`.cade` or `.envrc`), then continues up through the contiguous chain of
   config directories (parent-first), stopping at the first gap
3. it keeps only the run of directories that are allowed in its SQLite
   database (`$XDG_STATE_HOME/cade/cade.db`), capping at the first unapproved
   ancestor, so untrusted layers above your approved base are dropped
4. it parses and loads each remaining layer's environment from the configured sources
5. layers merge and cade emits shell-specific, safely-quoted commands to stdout
6. your shell evaluates the output, setting/unsetting variables and running hooks
7. on exit it restores precisely what it changed: variables cade set are
   reverted to their prior value or unset, while shell-managed variables
   (`PWD`, `OLDPWD`, `SHLVL`, status, …) and anything you changed mid-session are left
   untouched. after `pure`, the discarded ambient environment is restored from a
   snapshot.

loaded layers are cached per directory and re-evaluated when a `.cade` file or
any input it references changes.

## license

EUPL-1.2. see [LICENSE](LICENSE)
