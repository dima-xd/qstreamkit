#define VK_USE_PLATFORM_ANDROID_KHR 1

#include <jni.h>
#include <android/native_window_jni.h>
#include <android/log.h>
#include <vulkan/vulkan.h>
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#include "shaders.h"

#define TAG "qprofprobe"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
#define MAX_IMAGES 16
#define OFF_W 1280
#define OFF_H 720

typedef struct {
    VkRenderPass pass;
    VkPipelineLayout layout;
    VkPipeline pipe;
} PipeSet;

typedef struct {
    VkSwapchainKHR chain;
    VkFormat format;
    VkExtent2D extent;
    uint32_t count;
    VkImage images[MAX_IMAGES];
    VkImageView views[MAX_IMAGES];
    VkFramebuffer fbs[MAX_IMAGES];
    PipeSet ps;
} Swap;

typedef struct {
    VkImage image;
    VkDeviceMemory mem;
    VkImageView view;
    VkFramebuffer fb;
    VkExtent2D extent;
    PipeSet ps;
} Off;

static VkInstance g_inst = VK_NULL_HANDLE;
static VkPhysicalDevice g_pd = VK_NULL_HANDLE;
static VkDevice g_dev = VK_NULL_HANDLE;
static VkQueue g_queue = VK_NULL_HANDLE;
static uint32_t g_qfam = 0;
static VkCommandPool g_pool = VK_NULL_HANDLE;
static VkCommandBuffer g_cmd = VK_NULL_HANDLE;
static VkSemaphore g_sem_avail = VK_NULL_HANDLE;
static VkSemaphore g_sem_done = VK_NULL_HANDLE;
static VkFence g_fence = VK_NULL_HANDLE;

static Swap g_sw;
static Off g_off;
static VkSurfaceKHR g_surf = VK_NULL_HANDLE;
static ANativeWindow *g_win = NULL;
static ANativeWindow *g_pending = NULL;

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static volatile int g_run = 0;
static volatile int g_active = 0;
static volatile int g_recreate = 0;
static volatile int g_surface_down = 0;
static volatile int g_surface_done = 1;
static pthread_t g_thread;
static int g_thread_started = 0;
static unsigned long g_frames = 0;
static int g_perfetto = 0;

static uint32_t mem_type(uint32_t bits, VkMemoryPropertyFlags props) {
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(g_pd, &mp);
    for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
        if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & props) == props) {
            return i;
        }
    }
    return 0;
}

static void destroy_swap(void) {
    if (g_dev == VK_NULL_HANDLE) {
        return;
    }
    vkDeviceWaitIdle(g_dev);
    for (uint32_t i = 0; i < g_sw.count; i++) {
        if (g_sw.fbs[i]) {
            vkDestroyFramebuffer(g_dev, g_sw.fbs[i], NULL);
        }
        if (g_sw.views[i]) {
            vkDestroyImageView(g_dev, g_sw.views[i], NULL);
        }
    }
    if (g_sw.ps.pipe) {
        vkDestroyPipeline(g_dev, g_sw.ps.pipe, NULL);
    }
    if (g_sw.ps.layout) {
        vkDestroyPipelineLayout(g_dev, g_sw.ps.layout, NULL);
    }
    if (g_sw.ps.pass) {
        vkDestroyRenderPass(g_dev, g_sw.ps.pass, NULL);
    }
    if (g_sw.chain) {
        vkDestroySwapchainKHR(g_dev, g_sw.chain, NULL);
    }
    memset(&g_sw, 0, sizeof(g_sw));
}

static VkSurfaceFormatKHR pick_format(uint32_t count, const VkSurfaceFormatKHR *list) {
    for (uint32_t i = 0; i < count; i++) {
        if (list[i].colorSpace != VK_COLOR_SPACE_SRGB_NONLINEAR_KHR) {
            continue;
        }
        if (list[i].format == VK_FORMAT_B8G8R8A8_UNORM ||
            list[i].format == VK_FORMAT_R8G8B8A8_UNORM) {
            return list[i];
        }
    }
    return list[0];
}

static int make_render_pass(VkFormat fmt, VkImageLayout final_layout, VkRenderPass *out) {
    VkAttachmentDescription att;
    memset(&att, 0, sizeof(att));
    att.format = fmt;
    att.samples = VK_SAMPLE_COUNT_1_BIT;
    att.loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR;
    att.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
    att.stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE;
    att.stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE;
    att.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    att.finalLayout = final_layout;

    VkAttachmentReference ref;
    ref.attachment = 0;
    ref.layout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;

    VkSubpassDescription sub;
    memset(&sub, 0, sizeof(sub));
    sub.pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS;
    sub.colorAttachmentCount = 1;
    sub.pColorAttachments = &ref;

    VkSubpassDependency dep;
    memset(&dep, 0, sizeof(dep));
    dep.srcSubpass = VK_SUBPASS_EXTERNAL;
    dep.dstSubpass = 0;
    dep.srcStageMask = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
    dep.dstStageMask = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
    dep.dstAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT;

    VkRenderPassCreateInfo ci;
    memset(&ci, 0, sizeof(ci));
    ci.sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO;
    ci.attachmentCount = 1;
    ci.pAttachments = &att;
    ci.subpassCount = 1;
    ci.pSubpasses = &sub;
    ci.dependencyCount = 1;
    ci.pDependencies = &dep;

    VkResult r = vkCreateRenderPass(g_dev, &ci, NULL, out);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateRenderPass -> %d", r);
        return -1;
    }
    return 0;
}

static int make_pipeline(VkRenderPass pass, PipeSet *ps) {
    VkShaderModule vs = VK_NULL_HANDLE;
    VkShaderModule fs = VK_NULL_HANDLE;

    VkShaderModuleCreateInfo si;
    memset(&si, 0, sizeof(si));
    si.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO;
    si.codeSize = sizeof(spv_vertex);
    si.pCode = spv_vertex;
    VkResult r = vkCreateShaderModule(g_dev, &si, NULL, &vs);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateShaderModule vertex -> %d", r);
        return -1;
    }

    si.codeSize = sizeof(spv_fragment);
    si.pCode = spv_fragment;
    r = vkCreateShaderModule(g_dev, &si, NULL, &fs);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateShaderModule fragment -> %d", r);
        vkDestroyShaderModule(g_dev, vs, NULL);
        return -1;
    }

    VkPipelineShaderStageCreateInfo stages[2];
    memset(stages, 0, sizeof(stages));
    stages[0].sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO;
    stages[0].stage = VK_SHADER_STAGE_VERTEX_BIT;
    stages[0].module = vs;
    stages[0].pName = "main";
    stages[1].sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO;
    stages[1].stage = VK_SHADER_STAGE_FRAGMENT_BIT;
    stages[1].module = fs;
    stages[1].pName = "main";

    VkPipelineVertexInputStateCreateInfo vi;
    memset(&vi, 0, sizeof(vi));
    vi.sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO;

    VkPipelineInputAssemblyStateCreateInfo ia;
    memset(&ia, 0, sizeof(ia));
    ia.sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO;
    ia.topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST;

    VkPipelineViewportStateCreateInfo vp;
    memset(&vp, 0, sizeof(vp));
    vp.sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO;
    vp.viewportCount = 1;
    vp.scissorCount = 1;

    VkPipelineRasterizationStateCreateInfo rs;
    memset(&rs, 0, sizeof(rs));
    rs.sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO;
    rs.polygonMode = VK_POLYGON_MODE_FILL;
    rs.cullMode = VK_CULL_MODE_NONE;
    rs.frontFace = VK_FRONT_FACE_COUNTER_CLOCKWISE;
    rs.lineWidth = 1.0f;

    VkPipelineMultisampleStateCreateInfo ms;
    memset(&ms, 0, sizeof(ms));
    ms.sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO;
    ms.rasterizationSamples = VK_SAMPLE_COUNT_1_BIT;

    VkPipelineColorBlendAttachmentState ba;
    memset(&ba, 0, sizeof(ba));
    ba.colorWriteMask = 0xf;

    VkPipelineColorBlendStateCreateInfo cb;
    memset(&cb, 0, sizeof(cb));
    cb.sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO;
    cb.attachmentCount = 1;
    cb.pAttachments = &ba;

    VkDynamicState dyn[2] = {VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR};
    VkPipelineDynamicStateCreateInfo ds;
    memset(&ds, 0, sizeof(ds));
    ds.sType = VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO;
    ds.dynamicStateCount = 2;
    ds.pDynamicStates = dyn;

    VkPipelineLayoutCreateInfo li;
    memset(&li, 0, sizeof(li));
    li.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO;
    r = vkCreatePipelineLayout(g_dev, &li, NULL, &ps->layout);
    if (r != VK_SUCCESS) {
        LOGI("vkCreatePipelineLayout -> %d", r);
        vkDestroyShaderModule(g_dev, vs, NULL);
        vkDestroyShaderModule(g_dev, fs, NULL);
        return -1;
    }

    VkGraphicsPipelineCreateInfo ci;
    memset(&ci, 0, sizeof(ci));
    ci.sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO;
    ci.stageCount = 2;
    ci.pStages = stages;
    ci.pVertexInputState = &vi;
    ci.pInputAssemblyState = &ia;
    ci.pViewportState = &vp;
    ci.pRasterizationState = &rs;
    ci.pMultisampleState = &ms;
    ci.pColorBlendState = &cb;
    ci.pDynamicState = &ds;
    ci.layout = ps->layout;
    ci.renderPass = pass;
    ci.subpass = 0;

    r = vkCreateGraphicsPipelines(g_dev, VK_NULL_HANDLE, 1, &ci, NULL, &ps->pipe);
    vkDestroyShaderModule(g_dev, vs, NULL);
    vkDestroyShaderModule(g_dev, fs, NULL);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateGraphicsPipelines -> %d", r);
        return -1;
    }
    return 0;
}

static int create_swap(void) {
    VkSurfaceCapabilitiesKHR caps;
    VkResult r = vkGetPhysicalDeviceSurfaceCapabilitiesKHR(g_pd, g_surf, &caps);
    if (r != VK_SUCCESS) {
        LOGI("surface caps -> %d", r);
        return -1;
    }

    uint32_t count = 0;
    vkGetPhysicalDeviceSurfaceFormatsKHR(g_pd, g_surf, &count, NULL);
    VkSurfaceFormatKHR formats[64];
    if (count > 64) {
        count = 64;
    }
    vkGetPhysicalDeviceSurfaceFormatsKHR(g_pd, g_surf, &count, formats);
    VkSurfaceFormatKHR fmt = pick_format(count, formats);

    VkExtent2D extent = caps.currentExtent;
    if (extent.width == UINT32_MAX) {
        int w = ANativeWindow_getWidth(g_win);
        int h = ANativeWindow_getHeight(g_win);
        if (w <= 0 || h <= 0) {
            return -1;
        }
        extent.width = (uint32_t)w;
        extent.height = (uint32_t)h;
        if (extent.width < caps.minImageExtent.width) {
            extent.width = caps.minImageExtent.width;
        }
        if (extent.width > caps.maxImageExtent.width) {
            extent.width = caps.maxImageExtent.width;
        }
        if (extent.height < caps.minImageExtent.height) {
            extent.height = caps.minImageExtent.height;
        }
        if (extent.height > caps.maxImageExtent.height) {
            extent.height = caps.maxImageExtent.height;
        }
    }
    if (extent.width == 0 || extent.height == 0) {
        return -1;
    }

    uint32_t want = caps.minImageCount + 1;
    if (caps.maxImageCount > 0 && want > caps.maxImageCount) {
        want = caps.maxImageCount;
    }

    VkCompositeAlphaFlagBitsKHR alpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR;
    if (!(caps.supportedCompositeAlpha & alpha)) {
        alpha = VK_COMPOSITE_ALPHA_INHERIT_BIT_KHR;
    }

    VkSwapchainCreateInfoKHR ci;
    memset(&ci, 0, sizeof(ci));
    ci.sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR;
    ci.surface = g_surf;
    ci.minImageCount = want;
    ci.imageFormat = fmt.format;
    ci.imageColorSpace = fmt.colorSpace;
    ci.imageExtent = extent;
    ci.imageArrayLayers = 1;
    ci.imageUsage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    ci.imageSharingMode = VK_SHARING_MODE_EXCLUSIVE;
    ci.preTransform = caps.currentTransform;
    ci.compositeAlpha = alpha;
    ci.presentMode = VK_PRESENT_MODE_FIFO_KHR;
    ci.clipped = VK_TRUE;

    r = vkCreateSwapchainKHR(g_dev, &ci, NULL, &g_sw.chain);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateSwapchainKHR -> %d", r);
        return -1;
    }

    g_sw.format = fmt.format;
    g_sw.extent = extent;
    vkGetSwapchainImagesKHR(g_dev, g_sw.chain, &g_sw.count, NULL);
    if (g_sw.count > MAX_IMAGES) {
        g_sw.count = MAX_IMAGES;
    }
    vkGetSwapchainImagesKHR(g_dev, g_sw.chain, &g_sw.count, g_sw.images);

    for (uint32_t i = 0; i < g_sw.count; i++) {
        VkImageViewCreateInfo vi;
        memset(&vi, 0, sizeof(vi));
        vi.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO;
        vi.image = g_sw.images[i];
        vi.viewType = VK_IMAGE_VIEW_TYPE_2D;
        vi.format = g_sw.format;
        vi.subresourceRange.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
        vi.subresourceRange.levelCount = 1;
        vi.subresourceRange.layerCount = 1;
        r = vkCreateImageView(g_dev, &vi, NULL, &g_sw.views[i]);
        if (r != VK_SUCCESS) {
            LOGI("vkCreateImageView -> %d", r);
            return -1;
        }
    }

    if (make_render_pass(g_sw.format, VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
                         &g_sw.ps.pass) != 0) {
        return -1;
    }
    if (make_pipeline(g_sw.ps.pass, &g_sw.ps) != 0) {
        return -1;
    }

    for (uint32_t i = 0; i < g_sw.count; i++) {
        VkFramebufferCreateInfo fb;
        memset(&fb, 0, sizeof(fb));
        fb.sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO;
        fb.renderPass = g_sw.ps.pass;
        fb.attachmentCount = 1;
        fb.pAttachments = &g_sw.views[i];
        fb.width = extent.width;
        fb.height = extent.height;
        fb.layers = 1;
        r = vkCreateFramebuffer(g_dev, &fb, NULL, &g_sw.fbs[i]);
        if (r != VK_SUCCESS) {
            LOGI("vkCreateFramebuffer -> %d", r);
            return -1;
        }
    }

    LOGI("swapchain %ux%u images=%u", extent.width, extent.height, g_sw.count);
    return 0;
}

static int init_offscreen(void) {
    g_off.extent.width = OFF_W;
    g_off.extent.height = OFF_H;

    VkImageCreateInfo ic;
    memset(&ic, 0, sizeof(ic));
    ic.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    ic.imageType = VK_IMAGE_TYPE_2D;
    ic.format = VK_FORMAT_R8G8B8A8_UNORM;
    ic.extent.width = OFF_W;
    ic.extent.height = OFF_H;
    ic.extent.depth = 1;
    ic.mipLevels = 1;
    ic.arrayLayers = 1;
    ic.samples = VK_SAMPLE_COUNT_1_BIT;
    ic.tiling = VK_IMAGE_TILING_OPTIMAL;
    ic.usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    ic.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    ic.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;

    VkResult r = vkCreateImage(g_dev, &ic, NULL, &g_off.image);
    if (r != VK_SUCCESS) {
        LOGI("offscreen vkCreateImage -> %d", r);
        return -1;
    }

    VkMemoryRequirements req;
    vkGetImageMemoryRequirements(g_dev, g_off.image, &req);

    VkMemoryAllocateInfo ai;
    memset(&ai, 0, sizeof(ai));
    ai.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    ai.allocationSize = req.size;
    ai.memoryTypeIndex = mem_type(req.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    r = vkAllocateMemory(g_dev, &ai, NULL, &g_off.mem);
    if (r != VK_SUCCESS) {
        LOGI("offscreen vkAllocateMemory -> %d", r);
        return -1;
    }
    vkBindImageMemory(g_dev, g_off.image, g_off.mem, 0);

    VkImageViewCreateInfo vi;
    memset(&vi, 0, sizeof(vi));
    vi.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO;
    vi.image = g_off.image;
    vi.viewType = VK_IMAGE_VIEW_TYPE_2D;
    vi.format = VK_FORMAT_R8G8B8A8_UNORM;
    vi.subresourceRange.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
    vi.subresourceRange.levelCount = 1;
    vi.subresourceRange.layerCount = 1;
    r = vkCreateImageView(g_dev, &vi, NULL, &g_off.view);
    if (r != VK_SUCCESS) {
        LOGI("offscreen vkCreateImageView -> %d", r);
        return -1;
    }

    if (make_render_pass(VK_FORMAT_R8G8B8A8_UNORM,
                         VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                         &g_off.ps.pass) != 0) {
        return -1;
    }
    if (make_pipeline(g_off.ps.pass, &g_off.ps) != 0) {
        return -1;
    }

    VkFramebufferCreateInfo fb;
    memset(&fb, 0, sizeof(fb));
    fb.sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO;
    fb.renderPass = g_off.ps.pass;
    fb.attachmentCount = 1;
    fb.pAttachments = &g_off.view;
    fb.width = OFF_W;
    fb.height = OFF_H;
    fb.layers = 1;
    r = vkCreateFramebuffer(g_dev, &fb, NULL, &g_off.fb);
    if (r != VK_SUCCESS) {
        LOGI("offscreen vkCreateFramebuffer -> %d", r);
        return -1;
    }

    LOGI("offscreen %dx%d ready", OFF_W, OFF_H);
    return 0;
}

static void record_cmd(VkRenderPass pass, VkFramebuffer fb, VkExtent2D extent,
                       VkPipeline pipe) {
    VkCommandBufferBeginInfo bi;
    memset(&bi, 0, sizeof(bi));
    bi.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    vkBeginCommandBuffer(g_cmd, &bi);

    VkClearValue clear;
    memset(&clear, 0, sizeof(clear));
    clear.color.float32[0] = 0.05f;
    clear.color.float32[1] = 0.05f;
    clear.color.float32[2] = 0.10f;
    clear.color.float32[3] = 1.0f;

    VkRenderPassBeginInfo rp;
    memset(&rp, 0, sizeof(rp));
    rp.sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO;
    rp.renderPass = pass;
    rp.framebuffer = fb;
    rp.renderArea.extent = extent;
    rp.clearValueCount = 1;
    rp.pClearValues = &clear;
    vkCmdBeginRenderPass(g_cmd, &rp, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(g_cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);

    VkViewport vp;
    vp.x = 0.0f;
    vp.y = 0.0f;
    vp.width = (float)extent.width;
    vp.height = (float)extent.height;
    vp.minDepth = 0.0f;
    vp.maxDepth = 1.0f;
    vkCmdSetViewport(g_cmd, 0, 1, &vp);

    VkRect2D sc;
    sc.offset.x = 0;
    sc.offset.y = 0;
    sc.extent = extent;
    vkCmdSetScissor(g_cmd, 0, 1, &sc);

    vkCmdDraw(g_cmd, 3, 1, 0, 0);
    vkCmdEndRenderPass(g_cmd);
    vkEndCommandBuffer(g_cmd);
}

static void draw_frame(void) {
    vkWaitForFences(g_dev, 1, &g_fence, VK_TRUE, UINT64_MAX);

    uint32_t index = 0;
    VkResult r = vkAcquireNextImageKHR(g_dev, g_sw.chain, 100000000ULL, g_sem_avail,
                                       VK_NULL_HANDLE, &index);
    if (r == VK_TIMEOUT) {
        return;
    }
    if (r == VK_ERROR_OUT_OF_DATE_KHR) {
        g_recreate = 1;
        return;
    }
    if (r != VK_SUCCESS && r != VK_SUBOPTIMAL_KHR) {
        g_recreate = 1;
        return;
    }

    vkResetFences(g_dev, 1, &g_fence);
    vkResetCommandBuffer(g_cmd, 0);
    record_cmd(g_sw.ps.pass, g_sw.fbs[index], g_sw.extent, g_sw.ps.pipe);

    VkPipelineStageFlags wait = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
    VkSubmitInfo si;
    memset(&si, 0, sizeof(si));
    si.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
    si.waitSemaphoreCount = 1;
    si.pWaitSemaphores = &g_sem_avail;
    si.pWaitDstStageMask = &wait;
    si.commandBufferCount = 1;
    si.pCommandBuffers = &g_cmd;
    si.signalSemaphoreCount = 1;
    si.pSignalSemaphores = &g_sem_done;
    r = vkQueueSubmit(g_queue, 1, &si, g_fence);
    if (r != VK_SUCCESS) {
        LOGI("vkQueueSubmit -> %d", r);
        g_recreate = 1;
        return;
    }

    VkPresentInfoKHR pi;
    memset(&pi, 0, sizeof(pi));
    pi.sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR;
    pi.waitSemaphoreCount = 1;
    pi.pWaitSemaphores = &g_sem_done;
    pi.swapchainCount = 1;
    pi.pSwapchains = &g_sw.chain;
    pi.pImageIndices = &index;
    r = vkQueuePresentKHR(g_queue, &pi);
    if (r == VK_ERROR_OUT_OF_DATE_KHR || r == VK_SUBOPTIMAL_KHR) {
        g_recreate = 1;
    }
}

static void draw_offscreen(void) {
    if (g_off.fb == VK_NULL_HANDLE) {
        return;
    }
    vkWaitForFences(g_dev, 1, &g_fence, VK_TRUE, UINT64_MAX);
    vkResetFences(g_dev, 1, &g_fence);
    vkResetCommandBuffer(g_cmd, 0);
    record_cmd(g_off.ps.pass, g_off.fb, g_off.extent, g_off.ps.pipe);

    VkSubmitInfo si;
    memset(&si, 0, sizeof(si));
    si.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
    si.commandBufferCount = 1;
    si.pCommandBuffers = &g_cmd;
    VkResult r = vkQueueSubmit(g_queue, 1, &si, g_fence);
    if (r != VK_SUCCESS) {
        LOGI("offscreen submit -> %d", r);
        return;
    }
    vkWaitForFences(g_dev, 1, &g_fence, VK_TRUE, UINT64_MAX);
    usleep(15000);
}

static int init_device(void) {
    VkApplicationInfo app;
    memset(&app, 0, sizeof(app));
    app.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO;
    app.pApplicationName = "qstream-stand";
    app.apiVersion = VK_API_VERSION_1_1;

    const char *layers[] = {"VK_LAYER_ADRENO_qprofiler"};
    const char *exts[] = {VK_KHR_SURFACE_EXTENSION_NAME,
                          VK_KHR_ANDROID_SURFACE_EXTENSION_NAME};

    VkInstanceCreateInfo ci;
    memset(&ci, 0, sizeof(ci));
    ci.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO;
    ci.pApplicationInfo = &app;
    ci.enabledLayerCount = 1;
    ci.ppEnabledLayerNames = layers;
    ci.enabledExtensionCount = 2;
    ci.ppEnabledExtensionNames = exts;

    VkResult r = vkCreateInstance(&ci, NULL, &g_inst);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateInstance -> %d", r);
        return -1;
    }

    uint32_t n = 0;
    vkEnumeratePhysicalDevices(g_inst, &n, NULL);
    if (n == 0) {
        LOGI("no physical devices");
        return -1;
    }
    vkEnumeratePhysicalDevices(g_inst, &n, &g_pd);

    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(g_pd, &props);
    LOGI("gpu: %s", props.deviceName);

    uint32_t qn = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(g_pd, &qn, NULL);
    VkQueueFamilyProperties qs[16];
    if (qn > 16) {
        qn = 16;
    }
    vkGetPhysicalDeviceQueueFamilyProperties(g_pd, &qn, qs);

    int found = -1;
    for (uint32_t i = 0; i < qn; i++) {
        if (!(qs[i].queueFlags & VK_QUEUE_GRAPHICS_BIT)) {
            continue;
        }
        if (g_surf == VK_NULL_HANDLE) {
            found = (int)i;
            break;
        }
        VkBool32 present = VK_FALSE;
        vkGetPhysicalDeviceSurfaceSupportKHR(g_pd, i, g_surf, &present);
        if (present) {
            found = (int)i;
            break;
        }
    }
    if (found < 0) {
        LOGI("no suitable queue");
        return -1;
    }
    g_qfam = (uint32_t)found;

    float prio = 1.0f;
    VkDeviceQueueCreateInfo q;
    memset(&q, 0, sizeof(q));
    q.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO;
    q.queueFamilyIndex = g_qfam;
    q.queueCount = 1;
    q.pQueuePriorities = &prio;

    const char *dexts[] = {VK_KHR_SWAPCHAIN_EXTENSION_NAME};
    VkDeviceCreateInfo dci;
    memset(&dci, 0, sizeof(dci));
    dci.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO;
    dci.queueCreateInfoCount = 1;
    dci.pQueueCreateInfos = &q;
    dci.enabledExtensionCount = 1;
    dci.ppEnabledExtensionNames = dexts;

    r = vkCreateDevice(g_pd, &dci, NULL, &g_dev);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateDevice -> %d", r);
        return -1;
    }
    vkGetDeviceQueue(g_dev, g_qfam, 0, &g_queue);
    LOGI("vkDevice=%p", (void *)g_dev);

    VkCommandPoolCreateInfo pc;
    memset(&pc, 0, sizeof(pc));
    pc.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
    pc.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    pc.queueFamilyIndex = g_qfam;
    r = vkCreateCommandPool(g_dev, &pc, NULL, &g_pool);
    if (r != VK_SUCCESS) {
        LOGI("vkCreateCommandPool -> %d", r);
        return -1;
    }

    VkCommandBufferAllocateInfo ai;
    memset(&ai, 0, sizeof(ai));
    ai.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
    ai.commandPool = g_pool;
    ai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    ai.commandBufferCount = 1;
    r = vkAllocateCommandBuffers(g_dev, &ai, &g_cmd);
    if (r != VK_SUCCESS) {
        LOGI("vkAllocateCommandBuffers -> %d", r);
        return -1;
    }

    VkSemaphoreCreateInfo sc;
    memset(&sc, 0, sizeof(sc));
    sc.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO;
    VkFenceCreateInfo fc;
    memset(&fc, 0, sizeof(fc));
    fc.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
    fc.flags = VK_FENCE_CREATE_SIGNALED_BIT;

    if (vkCreateSemaphore(g_dev, &sc, NULL, &g_sem_avail) != VK_SUCCESS ||
        vkCreateSemaphore(g_dev, &sc, NULL, &g_sem_done) != VK_SUCCESS ||
        vkCreateFence(g_dev, &fc, NULL, &g_fence) != VK_SUCCESS) {
        LOGI("sync objects failed");
        return -1;
    }

    return 0;
}

static void teardown_surface(void) {
    if (g_dev != VK_NULL_HANDLE) {
        destroy_swap();
        if (g_surf) {
            vkDestroySurfaceKHR(g_inst, g_surf, NULL);
            g_surf = VK_NULL_HANDLE;
        }
    }
    if (g_win) {
        ANativeWindow_release(g_win);
        g_win = NULL;
    }
}

static ANativeWindow *take_pending(void) {
    ANativeWindow *win = NULL;
    pthread_mutex_lock(&g_lock);
    if (g_pending) {
        win = g_pending;
        g_pending = NULL;
    }
    pthread_mutex_unlock(&g_lock);
    return win;
}

static void enable_perfetto_mode(void) {
    void *h = dlopen("libVkLayer_ADRENO_qprofiler.so", RTLD_NOW | RTLD_GLOBAL);
    if (!h) {
        LOGI("perfetto: dlopen layer failed: %s", dlerror());
        return;
    }
    int (*setopts)(const void *) = (int (*)(const void *))dlsym(h, "profilerSetOptions");
    if (!setopts) {
        LOGI("perfetto: no profilerSetOptions");
        return;
    }
    unsigned int opts[4] = {0, 1, 1, 0};
    int r = setopts(opts);
    LOGI("perfetto: profilerSetOptions -> %d", r);
}

static void *render_main(void *arg) {
    (void)arg;

    if (g_perfetto) {
        enable_perfetto_mode();
    }
    if (init_device() != 0) {
        g_active = 0;
        return NULL;
    }
    init_offscreen();

    while (g_run) {
        ANativeWindow *win = take_pending();
        if (win) {
            teardown_surface();
            g_win = win;
            VkAndroidSurfaceCreateInfoKHR sci;
            memset(&sci, 0, sizeof(sci));
            sci.sType = VK_STRUCTURE_TYPE_ANDROID_SURFACE_CREATE_INFO_KHR;
            sci.window = g_win;
            if (vkCreateAndroidSurfaceKHR(g_inst, &sci, NULL, &g_surf) == VK_SUCCESS &&
                create_swap() == 0) {
                LOGI("rendering on surface pid=%d", (int)getpid());
            } else {
                LOGI("surface setup failed, staying offscreen");
                teardown_surface();
            }
            pthread_mutex_lock(&g_lock);
            g_surface_done = 1;
            pthread_mutex_unlock(&g_lock);
        }

        if (g_surface_down) {
            teardown_surface();
            pthread_mutex_lock(&g_lock);
            g_surface_down = 0;
            g_surface_done = 1;
            pthread_mutex_unlock(&g_lock);
            LOGI("surface detached, offscreen only");
        }

        if (g_sw.chain != VK_NULL_HANDLE) {
            if (g_recreate) {
                destroy_swap();
                if (create_swap() != 0) {
                    teardown_surface();
                }
                g_recreate = 0;
            } else {
                draw_frame();
            }
        } else {
            draw_offscreen();
        }

        g_frames++;
        if ((g_frames % 300) == 0) {
            LOGI("frames %lu", g_frames);
        }
    }

    teardown_surface();
    g_active = 0;
    return NULL;
}

JNIEXPORT void JNICALL
Java_com_example_qprofprobe_MainActivity_nativeSetPerfetto(JNIEnv *env, jobject thiz,
                                                          jboolean on) {
    (void)env;
    (void)thiz;
    g_perfetto = on ? 1 : 0;
}

JNIEXPORT void JNICALL
Java_com_example_qprofprobe_MainActivity_nativeStart(JNIEnv *env, jobject thiz,
                                                     jobject surface) {
    (void)thiz;
    if (surface == NULL) {
        return;
    }
    ANativeWindow *win = ANativeWindow_fromSurface(env, surface);
    if (!win) {
        LOGI("no native window");
        return;
    }

    pthread_mutex_lock(&g_lock);
    if (g_pending) {
        ANativeWindow_release(g_pending);
    }
    g_pending = win;
    g_surface_down = 0;
    g_surface_done = 0;
    pthread_mutex_unlock(&g_lock);

    g_run = 1;
    if (!g_active) {
        g_active = 1;
        if (pthread_create(&g_thread, NULL, render_main, NULL) != 0) {
            LOGI("pthread_create failed");
            g_active = 0;
            g_run = 0;
            pthread_mutex_lock(&g_lock);
            ANativeWindow_release(g_pending);
            g_pending = NULL;
            pthread_mutex_unlock(&g_lock);
        } else {
            g_thread_started = 1;
        }
    }
}

JNIEXPORT void JNICALL
Java_com_example_qprofprobe_MainActivity_nativeStop(JNIEnv *env, jobject thiz) {
    (void)env;
    (void)thiz;

    pthread_mutex_lock(&g_lock);
    g_surface_down = 1;
    g_surface_done = 0;
    if (g_pending) {
        ANativeWindow_release(g_pending);
        g_pending = NULL;
    }
    pthread_mutex_unlock(&g_lock);

    for (int i = 0; i < 200; i++) {
        pthread_mutex_lock(&g_lock);
        int done = g_surface_done;
        pthread_mutex_unlock(&g_lock);
        if (done) {
            break;
        }
        usleep(10000);
    }
}
