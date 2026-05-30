#!@nu@/bin/nu
# direnv shim, nushell flavour. behaviour matches direnv-compat.bash: relay
# cade's own json/shell output so no json is serialised here

def target-dir [target: string] {
  if ($target | is-empty) {
    "."
  } else {
    let expanded = ($target | path expand)
    if (($expanded | path type) == "dir") {
      $expanded
    } else {
      $expanded | path dirname
    }
  }
}

def --wrapped main [cmd?: string = "", target?: string = "", ...rest: string] {
  match $cmd {
    "export" => {
      let want = (if ($target | is-empty) { "bash" } else { $target })
      match $want {
        "json" => {
          let out = (^@cade@ reload --shell json)
          if ($out | is-empty) { "{}" } else { $out }
        }
        "bash" | "zsh" | "fish" | "nushell" | "nu" => {
          ^@cade@ reload --shell $want
        }
        _ => {}
      }
    }
    "hook" => {
      ^@cade@ hook (if ($target | is-empty) { "bash" } else { $target })
    }
    "allow" | "permit" | "grant" => {
      cd (target-dir $target)
      ^@cade@ allow
    }
    "deny" | "block" | "revoke" => {
      cd (target-dir $target)
      ^@cade@ disallow
    }
    "status" => {
      if $target == "--json" {
        print -e "direnv shim: status --json is unsupported"
        exit 1
      }
      ^@cade@ status
    }
    "version" => { print "2.34.0" }
    _ => {}
  }
}
