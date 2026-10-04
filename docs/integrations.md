# Dashboards and integrations

RustyBox answers two small JSON endpoints that dashboards such as [Glance](https://github.com/glanceapp/glance) or Homepage can read. Neither writes anything, and neither returns a secret.

| Endpoint | What it returns |
|---|---|
| `GET /api/summary` | Version, number of games and their total size, libraries, games held twice in one library, games without cover art, running jobs (title, percentage, step), jobs that failed in the last day, download counts (active, importing, failed, imported), wanted games waiting, and the 12 most recently added games with their cover file names. It only reads the database, so it is quick. |
| `GET /api/torrents/live` | What qBittorrent is doing right now: whether it is reachable, its version, download and upload speed, free space in its download folder, counts (downloading, seeding, paused, errored), and the busiest eight torrents with progress. When qBittorrent isn't set up or can't be reached it still answers 200 with `online: false` and a message. |

Cover images are served at `/api/covers/<file name>`. If you have turned on a login, a dashboard can't read these without a session; leave the login off on a trusted network or put the dashboard behind the same reverse proxy.

## A Glance example

```yaml
- type: custom-api
  title: RustyBox
  title-url: http://your-server:8088
  cache: 30s
  url: http://your-server:8088/api/summary
  template: |
    <div class="size-h3">{{ .JSON.Int "games" }} games</div>
    <div class="size-h6">{{ .JSON.Int "duplicates" }} duplicates · {{ .JSON.Int "wanted.waiting" }} wanted</div>
```

Cover strip, running-job bars and the qBittorrent speed readout are done the same way with `latest`, `jobs.running` and `/api/torrents/live`.
