#!/bin/sh
set -eu
install -d -o postgres -g postgres -m 700 /var/lib/postgresql/patroni
exec gosu postgres /opt/patroni/bin/patroni /etc/patroni.yml
