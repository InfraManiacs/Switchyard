# The Switchyard Admin console: a static page plus the nginx that serves it and
# forwards /v1/ to a switchyard-server.
#
# nginx-unprivileged rather than the stock nginx image, because this runs beside
# the server in a Kubernetes pod, where a container that does not need root
# should not ask for it. It listens on 8080 as uid 101 and carries the same
# template mechanism as the official image.
FROM nginxinc/nginx-unprivileged:1.29-alpine

# Where to send /v1/. Override for the deployment: a sidecar uses
# http://127.0.0.1:4000, a separate pod uses the server's Service name.
ENV SWITCHYARD_UPSTREAM=http://switchyard:4000 \
    NGINX_ENVSUBST_FILTER=^SWITCHYARD_

COPY default.conf.template /etc/nginx/templates/default.conf.template
COPY index.html app.js /usr/share/nginx/html/

EXPOSE 8080
