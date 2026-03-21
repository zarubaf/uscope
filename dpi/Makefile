CC ?= cc
CFLAGS ?= -O2 -Wall -Wextra -std=c99 -pedantic
AR ?= ar

SRCS = uscope_dpi.c lz4.c
OBJS = uscope_dpi.o lz4.o

.PHONY: all clean test

all: libuscope_dpi.a

libuscope_dpi.a: $(OBJS)
	$(AR) rcs $@ $^

uscope_dpi.o: uscope_dpi.c uscope_dpi.h lz4.h
	$(CC) $(CFLAGS) -Wno-unused-function -c -o $@ $<

lz4.o: lz4.c lz4.h
	$(CC) $(CFLAGS) -Wno-unused-function -c -o $@ $<

test_write: test_write.c libuscope_dpi.a
	$(CC) $(CFLAGS) -o $@ $< -L. -luscope_dpi

test: test_write
	./test_write
	@echo "C writer test passed. Verify output with: cargo test --test cross_validate"

clean:
	rm -f $(OBJS) libuscope_dpi.a test_write test_output.uscope
