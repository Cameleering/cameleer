// modzip packs a module from a git checkout into a GOPROXY file tree
// (<out>/<module>/@v/<version>.{zip,mod,info}).
//
// proxy.golang.org serves large module zips from storage.googleapis.com, which the test
// environment cannot reach. scripts/simulators.sh fetches such modules from their git
// repositories instead and builds with GOPROXY=file://<out>,https://proxy.golang.org: the go
// command still checks every zip against the go.sum of the program being built, so a zip made
// here is only accepted if it is byte-for-byte the module the upstream authors pinned.
//
//	modzip <git-dir (bare or not)> <revision> <module-path> <version> <out-dir>
package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"

	"golang.org/x/mod/module"
	"golang.org/x/mod/zip"
)

func main() {
	if len(os.Args) != 6 {
		fmt.Fprintln(os.Stderr, "usage: modzip <git-dir> <revision> <module-path> <version> <out-dir>")
		os.Exit(2)
	}
	repo, rev, path, ver, out := os.Args[1], os.Args[2], os.Args[3], os.Args[4], os.Args[5]
	esc, err := module.EscapePath(path)
	check(err)
	dir := filepath.Join(out, esc, "@v")
	check(os.MkdirAll(dir, 0o755))
	f, err := os.Create(filepath.Join(dir, ver+".zip"))
	check(err)
	// the go command's vcs cache holds bare repositories; CreateFromVCS wants a work tree
	tmp, err := os.MkdirTemp("", "modzip")
	check(err)
	defer os.RemoveAll(tmp)
	clone := exec.Command("git", "clone", "-q", "--shared", "--no-checkout", repo, tmp)
	clone.Stderr = os.Stderr
	check(clone.Run())
	check(zip.CreateFromVCS(f, module.Version{Path: path, Version: ver}, tmp, rev, ""))
	check(f.Close())
	check(os.WriteFile(filepath.Join(dir, ver+".info"), []byte(fmt.Sprintf(`{"Version":%q}`, ver)), 0o644))
	fmt.Println(filepath.Join(dir, ver+".zip"))
}

func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
