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

/* M24.3: an EXACT, unmodified copy of the pinned cFE tree's own
 * third_party/cfs/cfe/cmake/sample_defs/cfe_perfids.h -- NOT a cFE/OSAL/PSP patch (question
 * 148's "record what had to be patched" does not apply here). Found by actually running the
 * cross-build, not by static reading: unlike every other per-module config header (which falls
 * back to a `cfe/modules/<mod>/config/default_*.h` automatically when the mission's own
 * `_defs/` directory does not supply one -- confirmed working for e.g. cfe_mission_cfg.h in this
 * exact build), `cfe_perfids.h` has NO such fallback (`cfe/modules/core_api/mission_build.cmake`
 * generates it via `generate_configfile_set`, which silently produces an EMPTY generated header
 * -- `#ifndef .../#define .../#endif`, no content -- when no mission-supplied file is found,
 * rather than erroring at configure time). The empty header compiles fine until the first
 * translation unit that actually USES one of the reserved core perf-ID constants:
 * `cfe/modules/es/fsw/src/cfe_es_perf.c:644`'s `EntryData.Data = (Marker | (EntryExit <<
 * CFE_MISSION_ES_PERF_EXIT_BIT));` failed with `'CFE_MISSION_ES_PERF_EXIT_BIT' undeclared`. This
 * is why the posix build (`services/cfs/build/`'s sample_defs override) never hits this: the
 * fetched bundle's own `sample_defs/cfe_perfids.h` (a copy of this exact file, pre-existing
 * there) already supplies it. Our new `rtems_zynqmp_defs/` mission-defs directory (M24_3_REPORT.md's
 * "Scope note") needs its own copy for the identical reason -- copied here verbatim, no app-specific
 * perf IDs added (none of io_lockstep/sch_lockstep/adcs currently call CFE_ES_PerfLogEntry/Add
 * with an app-specific marker beyond entry ID 0, which IO_LOCKSTEP_AppMain already uses and
 * which needs no reservation of its own here).
 */

/**
 * @file
 *
 * Purpose: This file contains the cFE performance IDs
 *
 * Design Notes:
 *   Each performance id is used to identify something that needs to be
 *   measured.  Performance ids are limited to the range of 0 to
 *   CFE_MISSION_ES_PERF_MAX_IDS - 1.  Any performance ids outside of this range
 *   will be ignored and will be flagged as an error.  Note that
 *   performance ids 0-31 are reserved for the cFE Core.
 *
 * References:
 *
 */

#ifndef SAMPLE_PERFIDS_H
#define SAMPLE_PERFIDS_H

#define CFE_MISSION_ES_PERF_EXIT_BIT 31 /**< \brief bit (31) is reserved by the perf utilities */

/** \name cFE Performance Monitor IDs (Reserved IDs 0-31) */
/** \{ */
#define CFE_MISSION_ES_MAIN_PERF_ID       1  /**< \brief Performance ID for Executive Services Task */
#define CFE_MISSION_EVS_MAIN_PERF_ID      2  /**< \brief Performance ID for Events Services Task */
#define CFE_MISSION_TBL_MAIN_PERF_ID      3  /**< \brief Performance ID for Table Services Task */
#define CFE_MISSION_SB_MAIN_PERF_ID       4  /**< \brief Performance ID for Software Bus Services Task */
#define CFE_MISSION_SB_MSG_LIM_PERF_ID    5  /**< \brief Performance ID for Software Bus Msg Limit Errors */
#define CFE_MISSION_SB_PIPE_OFLOW_PERF_ID 27 /**< \brief Performance ID for Software Bus Pipe Overflow Errors */

#define CFE_MISSION_TIME_MAIN_PERF_ID        6 /**< \brief Performance ID for Time Services Task */
#define CFE_MISSION_TIME_TONE1HZISR_PERF_ID  7 /**< \brief Performance ID for 1 Hz Tone ISR */
#define CFE_MISSION_TIME_LOCAL1HZISR_PERF_ID 8 /**< \brief Performance ID for 1 Hz Local ISR */

#define CFE_MISSION_TIME_SENDMET_PERF_ID      9  /**< \brief Performance ID for Time ToneSendMET */
#define CFE_MISSION_TIME_LOCAL1HZTASK_PERF_ID 10 /**< \brief Performance ID for 1 Hz Local Task */
#define CFE_MISSION_TIME_TONE1HZTASK_PERF_ID  11 /**< \brief Performance ID for 1 Hz Tone Task */

/** \} */

#endif /* SAMPLE_PERFIDS_H */
