// SPDX-License-Identifier: GPL-3.0-or-later
// rootbox runs a standard sing-box config with root privileges. The supervisor
// owns the lifecycle: this process only reports readiness and traffic, and
// exits when the stop file appears or the supervisor dies.
package main

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"syscall"

	box "github.com/sagernet/sing-box"
	"github.com/sagernet/sing-box/experimental/v2rayapi"
	"github.com/sagernet/sing-box/include"
	"github.com/sagernet/sing-box/option"
)

func main() {
	// `--check` decodes and constructs the box without starting it, so a bad
	// config is rejected before anything touches the network.
	if len(os.Args) == 4 && os.Args[1] == "--check" {
		if err := check(os.Args[2], os.Args[3]); err != nil {
			fail(err)
		}
		return
	}
	if len(os.Args) != 7 {
		fmt.Fprintln(os.Stderr, "usage: rootbox <config> <assets> <pid> <ready> <stop> <supervisor-pid>")
		fmt.Fprintln(os.Stderr, "       rootbox --check <config> <assets>")
		os.Exit(2)
	}
	supervisorPID, err := strconv.Atoi(os.Args[6])
	if err != nil {
		fail(err)
	}
	if err = run(os.Args[1], os.Args[2], os.Args[3], os.Args[4], os.Args[5], supervisorPID); err != nil {
		fail(err)
	}
}

func check(configPath, assetsPath string) error {
	instance, _, err := newBox(configPath, assetsPath)
	if err != nil {
		return err
	}
	return instance.Close()
}

// newBox decodes the config and constructs the box; nothing is started.
func newBox(configPath, assetsPath string) (*box.Box, option.Options, error) {
	if err := os.Chdir(filepath.Dir(configPath)); err != nil {
		return nil, option.Options{}, err
	}
	_ = assetsPath
	var options option.Options
	boxCtx := include.Context(context.Background())
	raw, err := os.ReadFile(configPath)
	if err != nil {
		return nil, option.Options{}, err
	}
	if err = options.UnmarshalJSONContext(boxCtx, raw); err != nil {
		return nil, option.Options{}, fmt.Errorf("decode config: %w", err)
	}
	instance, err := box.New(box.Options{Options: options, Context: boxCtx})
	if err != nil {
		return nil, option.Options{}, err
	}
	return instance, options, nil
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, "rootbox:", err)
	os.Exit(1)
}

func run(configPath, assetsPath, pidPath, readyPath, stopPath string, supervisorPID int) error {
	if os.Geteuid() != 0 {
		return fmt.Errorf("root required")
	}
	if supervisorPID <= 1 {
		return fmt.Errorf("invalid supervisor pid")
	}
	if err := os.Chdir(filepath.Dir(configPath)); err != nil {
		return err
	}
	if err := os.WriteFile(pidPath, []byte(strconv.Itoa(os.Getpid())), 0o600); err != nil {
		return err
	}
	defer os.Remove(pidPath)
	if ca, err := os.ReadFile(filepath.Join(assetsPath, "ca.pem")); err == nil {
		updateRootCACerts(ca)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer cancel()

	instance, options, err := newBox(configPath, assetsPath)
	if err != nil {
		return err
	}
	defer instance.Close()

	metrics := &sampler{stats: v2rayapi.NewStatsService(option.V2RayStatsServiceOptions{
		Enabled:   true,
		Outbounds: outboundTags(options.Outbounds, options.Endpoints),
	})}
	instance.Router().AppendTracker(metrics.stats)
	if err = instance.Start(); err != nil {
		return err
	}
	if err = os.WriteFile(readyPath, []byte("ready"), 0o600); err != nil {
		return err
	}
	defer os.Remove(readyPath)

	go metrics.report(ctx, options.Outbounds, options.Endpoints, stopPath, supervisorPID)
	<-ctx.Done()
	return nil
}
