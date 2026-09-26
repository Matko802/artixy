#ifndef ARTIXY_SCRUB_H
#define ARTIXY_SCRUB_H

#include <stddef.h>

/* Replace public IPv4/IPv6 literals with "[redacted]". Returns malloc'd. */
char *scrub_public_ip(const char *s);

#endif
