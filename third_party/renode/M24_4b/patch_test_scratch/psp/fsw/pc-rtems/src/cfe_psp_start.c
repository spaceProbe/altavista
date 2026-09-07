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

/******************************************************************************
** File:  cfe_psp_start.c
**
** Purpose:
**   cFE BSP main entry point.
**
**
******************************************************************************/

/*
**  Include Files
*/
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <rtems.h>

/*
** cFE includes
*/
#include "common_types.h"
#include "osapi.h"
#include "cfe_psp.h"
#include "cfe_psp_memory.h"
#include "cfe_psp_module.h"

/*
 * The preferred way to obtain the CFE tunable values at runtime is via
 * the dynamically generated configuration object.  This allows a single build
 * of the PSP to be completely CFE-independent.
 */
#include "target_config.h"

#define CFE_PSP_MAIN_FUNCTION       (*GLOBAL_CONFIGDATA.CfeConfig->SystemMain)
#define CFE_PSP_NONVOL_STARTUP_FILE (GLOBAL_CONFIGDATA.CfeConfig->NonvolStartupFile)

/*
** Global variables
*/

rtems_id RtemsTimerId;

/*
** 1 HZ Timer "ISR"
*/
int timer_count = 0;

/******************************************************************************
**
**  Purpose:
**    Perform initial setup.
**
**    This function is invoked before OSAL is initialized.
**      NO OSAL CALLS SHOULD BE USED YET.
**
**    The root file system is created, and mount points are created and mounted:
**     - /ram as ramdisk (RFS), read-write
**     - /boot from /dev/hda1, read-only, contain the boot executable(s) (CFE core)
**
**  Arguments:
**    (none)
**
**  Return:
**    OS error code.  RTEMS_SUCCESSFUL if everything worked.
**
**  Note:
**    If this fails then CFE will not run properly, so a non-success here should
**    stop the boot so the issue can be fixed.  Trying to continue booting usually
**    just obfuscates the issue when something does not work later on.
*/
int CFE_PSP_Setup(void)
{
    return RTEMS_SUCCESSFUL;
}

/*
** A simple entry point to start from the BSP loader
**
** This entry point is used when building an RTEMS+CFE monolithic
** image, which is a single executable containing the RTEMS
** kernel and Core Flight Executive in one file.  In this mode
** the RTEMS BSP invokes the "Init" function directly.
**
** This sets up the root fs and the shell prior to invoking CFE via
** the CFE_PSP_Main() routine.
**
** In a future version this code may be moved into a separate bsp
** integration unit to be more symmetric with the VxWorks implementation.
*/
void OS_Application_Startup(void)
{
    if (CFE_PSP_Setup() != RTEMS_SUCCESSFUL)
    {
        CFE_PSP_Panic(CFE_PSP_ERROR); /* Unreachable currently - CFE_PSP_Setup always returns RTEMS_SUCCESSFUL */
    }

    /*
    ** Run the PSP Main - this will return when init is complete
    */
    CFE_PSP_Main();
}

/******************************************************************************
**
**  Purpose:
**    Application entry point.
**
**    The basic RTEMS system including the root FS and shell (if used) should
**    be running prior to invoking this function.
**
**    This entry point is used when building a separate RTEMS kernel/platform
**    boot image and Core Flight Executive image.  This is the type of deployment
**    used on e.g. VxWorks platforms.
**
**  Arguments:
**    (none)
**
**  Return:
**    (none)
*/

void CFE_PSP_Main(void)
{
    uint32    reset_type;
    uint32    reset_subtype;
    osal_id_t fs_id;
    int32     Status;

    /*
    ** Initialize the OS API
    */
    Status = OS_API_Init();
    if (Status != OS_SUCCESS)
    {
        /* irrecoverable error if OS_API_Init() fails. */
        /* note: use printf here, as OS_printf may not work */
        printf("CFE_PSP: OS_API_Init() failure\n");
        CFE_PSP_Panic(Status);
    }

    /*
     * Initialize the CFE reserved memory map
     */
    CFE_PSP_SetupReservedMemoryMap();

    /*
    ** Set up the virtual FS mapping for the "/cf" directory
    */
    Status = OS_FileSysAddFixedMap(&fs_id, "/mnt/eeprom", "/cf");
    if (Status != OS_SUCCESS)
    {
        /* Print for informational purposes --
         * startup can continue, but loads may fail later, depending on config. */
        OS_printf("CFE_PSP: OS_FileSysAddFixedMap() failure: %d\n", (int)Status);
    }

    /*
    ** AltaVista M24.4 (docs/open-questions.md question 148; recorded here as this task's
    ** first-ever patch to third_party/cfs/{cfe,osal,psp} -- every prior M24 task kept that
    ** count at zero): a raw ELF `LoadELF` boot under Renode has no real disk/network backing
    ** the "/cf" mapping just above, so CFE_ES_StartApplications's later open() of
    ** CFE_PSP_NONVOL_STARTUP_FILE ("/cf/cfe_es_startup.scr") fails with EC=-1 and no app ever
    ** starts, even though every app is already statically linked in (cpu1_STATIC_SYMLIST) --
    ** confirmed live, third_party/renode/M24_4_REPORT.md item 3. The exact same three lines
    ** services/cfs/build/generate_startup.cmake writes to disk for a real/posix build are
    ** written here directly into the default in-memory root filesystem via plain OSAL POSIX
    ** calls -- no new filesystem code, no tar/bin2c embedding needed: RTEMS's IMFS is already
    ** this BSP's unconditional default root filesystem, "/cf" already resolves into it via the
    ** OS_FileSysAddFixedMap call directly above, and OS_OpenCreate(..., OS_FILE_FLAG_CREATE) on
    ** that path is ordinary in-memory file creation, not a mount operation. Content copied
    ** byte-for-byte from a real cross-build's own generated file
    ** (third_party/cfs/build-rtems_zynqmp/exe/cpu1/eeprom/cfe_es_startup.scr), not retyped from
    ** memory. Guarded so a real disk-backed deployment (a future non-Renode target where "/cf"
    ** is persistent EEPROM/flash and may already carry a real, possibly different, startup
    ** file) is never overwritten: this only ever writes when the file cannot already be opened
    ** for reading.
    */
    {
        static const char av_m24_4_startup_script[] =
            "CFE_APP, io_lockstep,  IO_LOCKSTEP_AppMain, IO_LOCKSTEP, 70,  131072, 0x0, 0;\n"
            "CFE_APP, sch_lockstep, SCH_LS_AppMain,       SCH_LOCKSTEP, 70,  131072, 0x0, 0;\n"
            "CFE_APP, adcs,         ADCS_AppMain,         ADCS,        71,  131072, 0x0, 0;\n";
        osal_id_t check_fd;

        if (OS_OpenCreate(&check_fd, "/cf/cfe_es_startup.scr", OS_FILE_FLAG_NONE, OS_READ_ONLY) == OS_SUCCESS)
        {
            OS_close(check_fd);
        }
        else
        {
            osal_id_t startup_fd;
            int32     write_status = OS_OpenCreate(&startup_fd, "/cf/cfe_es_startup.scr", OS_FILE_FLAG_CREATE | OS_FILE_FLAG_TRUNCATE, OS_WRITE_ONLY);

            if (write_status == OS_SUCCESS)
            {
                OS_write(startup_fd, av_m24_4_startup_script, sizeof(av_m24_4_startup_script) - 1);
                OS_close(startup_fd);
                OS_printf("CFE_PSP: AltaVista M24.4 wrote a default /cf/cfe_es_startup.scr into the in-memory root fs (Renode/no-disk boot)\n");
            }
            else
            {
                OS_printf("CFE_PSP: AltaVista M24.4 could not write /cf/cfe_es_startup.scr: %d\n", (int)write_status);
            }
        }
    }

    /*
    ** Initialize the statically linked modules (if any)
    */
    CFE_PSP_ModuleInit();

    /*
    ** Determine Reset type by reading the hardware reset register.
    */
    reset_type    = CFE_PSP_RST_TYPE_POWERON;
    reset_subtype = CFE_PSP_RST_SUBTYPE_POWER_CYCLE;

    /*
    ** Initialize the reserved memory
    */
    CFE_PSP_InitProcessorReservedMemory(reset_type);

    /*
    ** Call cFE entry point. This will return when cFE startup
    ** is complete.
    */
    CFE_PSP_MAIN_FUNCTION(reset_type, reset_subtype, 1, CFE_PSP_NONVOL_STARTUP_FILE);
}
