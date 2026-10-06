/* Optional independent interoperability harness. MIT OR Apache-2.0.
 * Compile against an installed libltc and a generated Tidkod SDK (see docs/ltc.md).
 * This executable is a test tool, not part of either shipped library.
 */
#include <ltc.h>
#include <tidkod.h>
#include <stdio.h>
#include <stdlib.h>
#define REQUIRE(x) do { if (!(x)) { fprintf(stderr,"line %d: %s\n",__LINE__,#x); exit(1); } } while (0)
int main(void) {
    unsigned rates[] = {44100,48000,96000};
    unsigned nums[] = {24000,24,25,30000,30000,30};
    unsigned dens[] = {1001,1,1,1001,1001,1};
    for (unsigned ri=0;ri<3;ri++) for (unsigned fi=0;fi<6;fi++) {
        unsigned sr=rates[ri],n=nums[fi],d=dens[fi]; int drop=fi==4;
        enum LTC_TV_STANDARD standard=fi==2?LTC_TV_625_50:fi<2?LTC_TV_FILM_24:LTC_TV_525_60;
        TKBuffer *error=NULL; LtcEncoder *encoder=NULL; LtcDecoder *decoder=NULL;
        REQUIRE(tidkod_ltc_encoder_new(n,d,drop,sr,0,&encoder,&error)==0);
        REQUIRE(tidkod_ltc_decoder_new(n,d,drop,sr,&decoder,&error)==0);
        REQUIRE(tidkod_ltc_encoder_set_metadata(encoder,0x87654321u,5,1,&error)==0);
        float *pcm=calloc(sr,sizeof(float)); REQUIRE(pcm);
        uint8_t state; REQUIRE(tidkod_ltc_encoder_render(encoder,pcm,sr,&state,&error)==0 && state==2);
        LTCDecoder *reference=ltc_decoder_create((int)((double)sr*d/n),64);REQUIRE(reference);
        ltc_decoder_write_float(reference,pcm,sr,0);
        LTCFrameExt frame;unsigned count=0;
        while(ltc_decoder_read(reference,&frame)) {
            SMPTETimecode tc;ltc_frame_to_time(&tc,&frame.ltc,0);
            REQUIRE(tc.hours==0 && tc.mins==0 && tc.secs==0);
            REQUIRE(frame.ltc.user1==1 && frame.ltc.user2==2 && frame.ltc.user3==3 && frame.ltc.user4==4);
            REQUIRE(frame.ltc.user5==5 && frame.ltc.user6==6 && frame.ltc.user7==7 && frame.ltc.user8==8);
            REQUIRE(frame.ltc.col_frame==1);
            REQUIRE(frame.ltc.dfbit==(unsigned)drop);
            REQUIRE(ltc_frame_parse_bcg_flags(&frame.ltc,standard)==5);
            /* Allow reference-decoder edge timing/acquisition rounding. */
            uint64_t boundary=(uint64_t)count*sr*d/n;
            REQUIRE(llabs((long long)frame.off_start-(long long)boundary)<=3);
            boundary=(uint64_t)(count+1)*sr*d/n;
            REQUIRE(llabs((long long)frame.off_end+1-(long long)boundary)<=3);
            REQUIRE(tc.frame==count);count++;
        }
        REQUIRE(count>=22);ltc_decoder_free(reference);
        // Independently generated, edge-shaped libltc audio -> Tidkod.
        LTCEncoder *reference_encoder=ltc_encoder_create(sr,(double)n/d,standard,0);REQUIRE(reference_encoder);
        SMPTETimecode tc={0};tc.hours=1;tc.mins=23;tc.secs=45;tc.frame=0;
        ltc_encoder_set_timecode(reference_encoder,&tc);
        LTCFrame codeword; ltc_encoder_get_frame(reference_encoder,&codeword);
        codeword.user1=1;codeword.user2=2;codeword.user3=3;codeword.user4=4;
        codeword.user5=5;codeword.user6=6;codeword.user7=7;codeword.user8=8;
        codeword.col_frame=1;
        codeword.binary_group_flag_bit1=1;
        if(standard==LTC_TV_625_50) {
            codeword.biphase_mark_phase_correction=1; /* BGF0 */
            codeword.binary_group_flag_bit0=1; /* BGF2 */
        } else {
            codeword.binary_group_flag_bit0=1;
            codeword.binary_group_flag_bit2=1;
        }
        codeword.dfbit=(unsigned)drop; ltc_frame_set_parity(&codeword,standard);
        ltc_encoder_set_frame(reference_encoder,&codeword);
        uint64_t offset=0,starts[36]={0};count=0;
        unsigned nominal=(n+d-1)/d;
        int64_t base=(1*3600+23*60+45)*nominal-(drop?2*(83-83/10):0);
        int64_t previous=-1;
        for(unsigned f=0;f<35;f++) {
            ltc_encoder_encode_frame(reference_encoder);unsigned char *bytes=NULL;
            int length=ltc_encoder_get_bufferptr(reference_encoder,&bytes,1);REQUIRE(length>0 && length<(int)sr);
            starts[f+1]=offset+(unsigned)length;
            for(int i=0;i<length;i++)pcm[i]=((float)bytes[i]-128.f)/128.f;
            size_t at=0;while(at<(size_t)length) {
                LtcResult result;
                REQUIRE(tidkod_ltc_decoder_process(decoder,pcm+at,length-at,offset+at,
                    1000000000ULL+(offset+at)*1000000000ULL/sr,&result,&error)==0);
                REQUIRE(result.consumed>0);at+=result.consumed;
                if(result.has_frame){
                    REQUIRE(result.polarity_valid);
                    int64_t index=result.frames-base;
                    REQUIRE(index>=0 && index<(int64_t)f);
                    REQUIRE(previous<0 ? index<=1 : index==previous+1);
                    previous=index;
                    REQUIRE(result.user_bits==0x87654321u);
                    REQUIRE(result.binary_group_flags==7 && result.color_frame);
                    /* libltc's default 40us edge shaping shifts threshold crossings. */
                    long long tolerance=(sr*40+999999)/1000000+1;
                    REQUIRE(llabs((long long)result.start_sample-(long long)starts[index])<=tolerance);
                    REQUIRE(llabs((long long)result.end_sample-(long long)starts[index+1])<=tolerance);
                    REQUIRE(llabs((long long)result.start_ns-(long long)(1000000000ULL+result.start_sample*1000000000ULL/sr))<=1);
                    REQUIRE(llabs((long long)result.end_ns-(long long)(1000000000ULL+result.end_sample*1000000000ULL/sr))<=1);
                    count++;
                }
            }
            offset+=(unsigned)length;ltc_encoder_inc_timecode(reference_encoder);
        }
        REQUIRE(count>=30);
        printf("%u/%u %s @ %u: bidirectional interoperability passed\n",n,d,drop?"DF":"NDF",sr);
        ltc_encoder_free(reference_encoder);tidkod_ltcencoder_free(encoder);tidkod_ltcdecoder_free(decoder);free(pcm);
    }
    return 0;
}
