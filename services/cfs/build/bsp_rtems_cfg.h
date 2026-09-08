/************************************************************************
 * NASA Docket No. GSC-19,200-1, and identified as "cFS Draco"
 *
 * Copyright (c) 2023 United States Government as represented by the
 * Administrator of the National Aeronautics and Space Administration.
 * All Rights Reserved.
 *
 * Licensed under the Apache License, Version 2.0 (the "License"); you may
 * not use this file except in compliance with the License. You may obtain
 * a copy of the License at http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 ************************************************************************/

/* M24.3: mission override of OSAL's own
 * third_party/cfs/osal/src/bsp/generic-rtems/config/default_bsp_rtems_cfg.h, which that file's
 * own top comment explicitly invites ("This file may be overridden/superseded by
 * mission-provided definitions by overriding this header"). Found running the cross-build, not
 * guessed: the default unconditionally sets `CONFIGURE_APPLICATION_NEEDS_IDE_DRIVER` and
 * `CONFIGURE_APPLICATION_NEEDS_ATA_DRIVER` -- real RTEMS `confdefs.h` options that register a
 * PC-style parallel-ATA disk driver (this generic-rtems BSP glue traces back to the "pc686"/
 * "pc-rtems" PC target, per psp/fsw/pc-rtems/README.txt's own setup instructions). The Zynq
 * UltraScale+ RPU target has no IDE/ATA controller at all, and linking `core-cpu1.exe` failed
 * with undefined references to `IDE_Controller_Table`/`IDE_Controller_Count` -- symbols a real
 * board-specific IDE driver table would supply, which this BSP has no reason to define. This
 * copy is IDENTICAL to the default except those two lines removed; everything else (task
 * counts, filesystem types, console/clock driver requirements) is unchanged. Not a cFE/OSAL/PSP
 * patch (question 148) -- an unmodified default this mission deliberately does not want, using
 * exactly the override mechanism the default file's own header documents.
 *
 * @note
 *   This file may be overridden/superseded by mission-provided definitions
 *   by overriding this header.
 */
#ifndef BSP_RTEMS_CFG_H
#define BSP_RTEMS_CFG_H

#include "osconfig.h"

#define TASK_INTLEVEL 0
#define CONFIGURE_INIT
#define CONFIGURE_INIT_TASK_ATTRIBUTES \
    (RTEMS_FLOATING_POINT | RTEMS_PREEMPT | RTEMS_NO_TIMESLICE | RTEMS_ASR | RTEMS_INTERRUPT_LEVEL(TASK_INTLEVEL))
#define CONFIGURE_INIT_TASK_STACK_SIZE (20 * 1024)
#define CONFIGURE_INIT_TASK_PRIORITY   10

/*
 * Note that these resources are shared with RTEMS itself (e.g. the init task, the shell)
 * so they should be allocated slightly higher than the user limits in osconfig.h
 *
 * Many RTEMS services use tasks internally, including the idle task, BSWP, ATA driver,
 * low level console I/O, the shell, TCP/IP network stack, and DHCP (if enabled).
 * Many of these also use semaphores for synchronization.
 *
 * Budgeting for additional:
 *   8 internal tasks
 *   2 internal timers
 *   4 internal queues
 *   16 internal semaphores
 *
 */
#define CONFIGURE_MAXIMUM_TASKS          (OS_MAX_TASKS + 8)
#define CONFIGURE_MAXIMUM_TIMERS         (OS_MAX_TIMERS + 2)
#define CONFIGURE_MAXIMUM_SEMAPHORES     (OS_MAX_BIN_SEMAPHORES + OS_MAX_COUNT_SEMAPHORES + OS_MAX_MUTEXES + 16)
#define CONFIGURE_MAXIMUM_MESSAGE_QUEUES (OS_MAX_QUEUES + 4)
#define CONFIGURE_MAXIMUM_DRIVERS        10
#define CONFIGURE_MAXIMUM_POSIX_KEYS     4
#ifdef OS_RTEMS_4_DEPRECATED
#define CONFIGURE_LIBIO_MAXIMUM_FILE_DESCRIPTORS (OS_MAX_NUM_OPEN_FILES + 8)
#else
#define CONFIGURE_MAXIMUM_FILE_DESCRIPTORS (OS_MAX_NUM_OPEN_FILES + 8)
#endif

#define CONFIGURE_RTEMS_INIT_TASKS_TABLE
#define CONFIGURE_APPLICATION_NEEDS_CONSOLE_DRIVER
#define CONFIGURE_APPLICATION_NEEDS_CLOCK_DRIVER
#define CONFIGURE_USE_IMFS_AS_BASE_FILESYSTEM
#define CONFIGURE_FILESYSTEM_RFS
#define CONFIGURE_FILESYSTEM_IMFS
#define CONFIGURE_FILESYSTEM_DOSFS
#define CONFIGURE_FILESYSTEM_DEVFS
#define CONFIGURE_APPLICATION_NEEDS_LIBBLOCK
/* M24.3: CONFIGURE_APPLICATION_NEEDS_IDE_DRIVER and CONFIGURE_APPLICATION_NEEDS_ATA_DRIVER
 * deliberately REMOVED here -- see this file's own top comment. zynqmp_rpu_lock_step has no
 * IDE/ATA hardware; those macros pulled in RTEMS's PC-ATA driver code
 * (bsps/shared/dev/ide/{ata,ide_controller}.c) expecting a board-specific IDE_Controller_Table
 * this BSP has no reason to supply. */

#define CONFIGURE_EXECUTIVE_RAM_SIZE       (8 * 1024 * 1024)
#define CONFIGURE_MICROSECONDS_PER_TICK    10000
#define CONFIGURE_ATA_DRIVER_TASK_PRIORITY 9

#endif
