#!/bin/bash

# This script creates a virtual loopback device in PulseAudio/PipeWire.
# The denoised audio from VoidMic will be sent to this "Sink".
# You can then use the "Monitor of VoidMic_Clean" as your microphone in Discord/OBS.

SINK_NAME="VoidMic_Clean"

if command -v pactl >/dev/null 2>&1; then
    echo "Creating virtual sink: $SINK_NAME"
    pactl load-module module-null-sink sink_name=$SINK_NAME sink_properties=device.description=$SINK_NAME
    echo "Done! You should now see '$SINK_NAME' in your sound settings."
    echo "In your application, select 'Monitor of $SINK_NAME' as your input device."
else
    echo "Error: 'pactl' not found. Please ensure PulseAudio or PipeWire-Pulse is installed."
    exit 1
fi
