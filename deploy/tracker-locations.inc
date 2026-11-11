proxy_http_version 1.1;
proxy_set_header Connection "";
proxy_set_header Host $host;
proxy_set_header X-Real-IP $remote_addr;
proxy_set_header X-Forwarded-For "";
proxy_set_header CF-Connecting-IP "";
proxy_connect_timeout 2s;
proxy_send_timeout 15s;
proxy_read_timeout 15s;

location ^~ /.well-known/acme-challenge/ {
    root /var/lib/letsencrypt;
    default_type text/plain;
}
location = /announce {
    if ($tracker_browser_announce) {
        add_header Cache-Control "no-store" always;
        add_header Vary Accept always;
        return 302 "https://tracker.augrains.com/?lang=$tracker_browser_lang#native-trackers";
    }
    proxy_cache off;
    proxy_pass http://tracker_core;
}
location = /scrape { proxy_cache off; proxy_pass http://tracker_core; }
location = /healthz { proxy_cache off; proxy_pass http://$tracker_health_backend; }
location = /readyz { proxy_cache off; proxy_pass http://$tracker_health_backend; }
location = /stats.json { proxy_pass http://tracker_core; }
location = /stats { proxy_pass http://tracker_core; }
location = /metrics { proxy_pass http://tracker_core; }
location ^~ /internal/ { return 404; }
location ^~ /releases/ {
    alias /opt/tracker-v1/public/releases/;
    autoindex off;
    index index.html;
    limit_except GET { deny all; }
    add_header X-Content-Type-Options nosniff always;
    add_header Cache-Control "public, max-age=86400, immutable";
}
location / { proxy_pass http://tracker_site; }
