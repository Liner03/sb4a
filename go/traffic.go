// SPDX-License-Identifier: GPL-3.0-or-later
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"syscall"
	"time"

	"github.com/sagernet/sing-box/experimental/v2rayapi"
	"github.com/sagernet/sing-box/option"
)

const statsPrefix = "WANBOX_STATS:"

type sampler struct {
	stats *v2rayapi.StatsService
}

type sample struct {
	Tag      string `json:"tag"`
	Tx       int64  `json:"tx"`
	Rx       int64  `json:"rx"`
	DirectTx int64  `json:"directTx"`
	DirectRx int64  `json:"directRx"`
}

// QueryStats resets the counters, so every routed tag is read once per sample.
func (s *sampler) query(tags []string, direction string, elapsed float64) int64 {
	if elapsed <= 0 || math.IsNaN(elapsed) || math.IsInf(elapsed, 0) {
		return 0
	}
	var bytes int64
	for _, tag := range tags {
		resp, err := s.stats.GetStats(context.Background(), &v2rayapi.GetStatsRequest{
			Name:   fmt.Sprintf("outbound>>>%s>>>traffic>>>%s", tag, direction),
			Reset_: true,
		})
		if err == nil && resp.Stat != nil {
			bytes += resp.Stat.Value
		}
	}
	return int64(float64(bytes) / elapsed)
}

func (s *sampler) report(ctx context.Context, outbounds []option.Outbound, endpoints []option.Endpoint, stopPath string, supervisorPID int) {
	proxy, direct := splitOutboundTags(outbounds, endpoints)
	tag := ""
	if len(proxy) > 0 {
		tag = proxy[0]
	} else if len(direct) > 0 {
		tag = direct[0]
	}
	last := time.Now()
	ticker := time.NewTicker(5 * time.Second) // same cadence as the supervisor's STATS_MIN_INTERVAL
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			if _, err := os.Stat(stopPath); err == nil || syscall.Kill(supervisorPID, 0) != nil {
				syscall.Kill(os.Getpid(), syscall.SIGTERM)
				return
			}
			elapsed := time.Since(last).Seconds()
			last = time.Now()
			payload, err := json.Marshal(sample{
				Tag:      tag,
				Tx:       s.query(proxy, "uplink", elapsed),
				Rx:       s.query(proxy, "downlink", elapsed),
				DirectTx: s.query(direct, "uplink", elapsed),
				DirectRx: s.query(direct, "downlink", elapsed),
			})
			if err == nil {
				fmt.Println(statsPrefix + string(payload))
			}
		}
	}
}

// splitOutboundTags keeps rule-selected outbounds visible, not only the main
// proxy alias. The stats service counts the routed outbound once, so detour
// hops are not summed twice.
func splitOutboundTags(outbounds []option.Outbound, endpoints []option.Endpoint) (proxy, direct []string) {
	seen := map[string]bool{}
	add := func(tag string, isDirect bool) {
		if tag == "" || seen[tag] {
			return
		}
		seen[tag] = true
		if isDirect {
			direct = append(direct, tag)
		} else {
			proxy = append(proxy, tag)
		}
	}
	for _, outbound := range outbounds {
		add(outbound.Tag, outbound.Type == "direct")
	}
	for _, endpoint := range endpoints {
		add(endpoint.Tag, false)
	}
	return proxy, direct
}

func outboundTags(outbounds []option.Outbound, endpoints []option.Endpoint) []string {
	proxy, direct := splitOutboundTags(outbounds, endpoints)
	return append(proxy, direct...)
}
