/* Test-only interoperability probe against the installed SPA implementation.
 * stdin is the Rust-serialized input offer; the peer reproduces Mutter 42's
 * observed BGRx/1280x720/variable-framerate format, without a running daemon.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <spa/param/video/format-utils.h>
#include <spa/pod/filter.h>

int main(int argc, char **argv)
{
    uint64_t input[512] = { 0 }, peer_data[512] = { 0 }, output[512] = { 0 };
    const size_t length = fread(input, 1, sizeof(input), stdin);
    const struct spa_pod *offer = (const struct spa_pod *)input;
    struct spa_pod_builder peer_builder = SPA_POD_BUILDER_INIT(peer_data, sizeof(peer_data));
    struct spa_pod_builder output_builder = SPA_POD_BUILDER_INIT(output, sizeof(output));
    const struct spa_rectangle size = SPA_RECTANGLE(1280, 720);
    const struct spa_fraction rate = SPA_FRACTION(0, 1);
    const struct spa_fraction max_rate = SPA_FRACTION(60, 1);
    const struct spa_fraction min_rate = SPA_FRACTION(1, 1);
    struct spa_pod *result = NULL;
    struct spa_video_info_raw video = { 0 };
    const uint32_t format = argc == 2 ? (uint32_t)strtoul(argv[1], NULL, 10) : SPA_VIDEO_FORMAT_BGRx;

    if (length < sizeof(*offer) || SPA_POD_SIZE(offer) != length)
        return 2;
    const struct spa_pod *peer = spa_pod_builder_add_object(&peer_builder,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format, SPA_POD_Id(format),
        SPA_FORMAT_VIDEO_size, SPA_POD_Rectangle(&size),
        SPA_FORMAT_VIDEO_framerate, SPA_POD_Fraction(&rate),
        SPA_FORMAT_VIDEO_maxFramerate, SPA_POD_CHOICE_RANGE_Fraction(&max_rate, &min_rate, &max_rate));
    int status = spa_pod_filter(&output_builder, &result, peer, offer);
    if (status < 0) {
        fprintf(stderr, "spa_pod_filter rejected the offer: %d\n", status);
        return 1;
    }
    if (spa_pod_fixate(result) < 0 || spa_format_video_raw_parse(result, &video) < 0)
        return 3;
    if (video.format != format || video.size.width != 1280 || video.size.height != 720
        || video.framerate.num != 0 || video.framerate.denom != 1)
        return 4;
    /* Return the actual fixed native POD so the Rust reader is tested too. */
    return fwrite(result, 1, SPA_POD_SIZE(result), stdout) == SPA_POD_SIZE(result) ? 0 : 5;
}
