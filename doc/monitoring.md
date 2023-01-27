## Monitoring

Indexing and serving metrics are exported via [Prometheus](https://github.com/tikv/rust-prometheus):

```bash
$ sudo apt install prometheus
```

Add `electrs` job to `scrape_configs` section in `/etc/prometheus/prometheus.yml`:

```
  - job_name: electrs
    static_configs:
      - targets: ['localhost:4224']
```

Restart and check the collected metrics:

```
$ sudo systemctl restart prometheus
$ firefox 'http://localhost:9090/graph?g0.range_input=1h&g0.expr=index_height&g0.tab=0'
```
