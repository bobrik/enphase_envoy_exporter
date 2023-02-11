# Enphase Envoy Prometheus exporter

Monitor your Enphase Envoy solar production in Prometheus.
Unlike Enphase Enlighten app or web dashboard, which aggregate on 15 minute
interval at a minimum, this exporter allows you to monitor by the second.

## Supported metrics

I only have production metrics, because my installer is greedy and wants $450
to install a $50 clamp to enable consumption monitoring.

### `enphase_envoy_production_watts`

Gauge for the current production power.

## Usage

This exporter is aimed for mostly local monitoring. It authenticates with the
Enphase mothership to get an auth token (valid for 1 year), and then only
communicated with your IQ Combiner / Gateway / whatever they call it now
that you have installed locally.

To build, simply use `cargo` from the cloned repo:

```
$ cargo build --release
```

To run, you need to figure out:

* IP address of your Gateway. This can be just `envoy.local` if you have working mDNS.
* Serial number of your Gateway. You can find this in the app.

Command line arguments:

```
$ ./target/release/enphase_envoy_exporter -h
Usage: enphase_envoy_exporter [OPTIONS] --envoy.address <ENVOY_ADDRESS> --envoy.serial <ENVOY_SERIAL> --envoy.username <ENVOY_USERNAME> --envoy.password <ENVOY_PASSWORD>

Options:
      --web.listen-address <LISTEN_ADDRESS>
          Address on which to expose metrics and web interface [default: [::1]:12345]
      --envoy.address <ENVOY_ADDRESS>
          Address of the Enphase Envoy on your local network
      --envoy.serial <ENVOY_SERIAL>
          Serial number of the Enphase Envoy (look up in the app)
      --envoy.username <ENVOY_USERNAME>
          Enphase Envoy username (look up in the app) [env: ENVOY_USERNAME=]
      --envoy.password <ENVOY_PASSWORD>
          Enphase Envoy username [env: ENVOY_PASSWORD=]
  -h, --help
          Print help
  -V, --version
          Print version
```

Running (substitute with your data):

```
$ ./target/release/enphase_envoy_exporter \
    --envoy.address 192.168.1.205 \
    --envoy.serial 2022XXXXXXXX \
    --envoy.username example@example.com \
    --envoy.password hunter2
```

Reading metrics:

```
$ curl http://localhost:12345/metrics
# HELP enphase_envoy_production_watts Currently produced watts.
# TYPE enphase_envoy_production_watts gauge
enphase_envoy_production_watts -0.0
# EOF
```
