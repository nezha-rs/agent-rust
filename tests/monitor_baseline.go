package main

import (
	"context"
	"encoding/json"
	"os"

	"github.com/nezhahq/agent/model"
	"github.com/nezhahq/agent/pkg/monitor"
	"github.com/nezhahq/agent/pkg/monitor/temperature"
)

func main() {
	config := &model.AgentConfig{}
	paths := os.Args[1:]
	temperatureProbe := false
	if len(paths) > 0 && paths[0] == "--gpu" {
		config.GPU = true
		paths = paths[1:]
	}
	if len(paths) > 0 && paths[0] == "--temperature" {
		config.Temperature = true
		temperatureProbe = true
		paths = paths[1:]
	}
	if len(paths) > 0 {
		config.HardDrivePartitionAllowlist = paths
	}
	host := monitor.GetHost(config)
	state := monitor.GetState(config, false, false)
	result := map[string]any{
		"host":  host,
		"state": state,
	}
	if temperatureProbe {
		values, err := temperature.GetState(context.Background())
		result["temperature_direct"] = values
		if err != nil {
			result["temperature_error"] = err.Error()
		}
	}
	_ = json.NewEncoder(os.Stdout).Encode(result)
}
