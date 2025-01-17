use crate::errors::OxenHttpError;
use crate::helpers::get_repo;
use crate::params::df_opts_query::{self, DFOptsQuery};
use crate::params::{app_data, parse_resource, path_param};

use liboxen::api;
use liboxen::core::cache::cachers;
use liboxen::error::OxenError;
use liboxen::model::{DataFrameSize, Schema};
use liboxen::opts::df_opts::DFOptsView;
use liboxen::view::entry::ResourceVersion;
use liboxen::view::json_data_frame_view::JsonDataFrameSource;
use liboxen::{constants, current_function};

use actix_web::{web, HttpRequest, HttpResponse};
use liboxen::core::df::tabular;
use liboxen::opts::{DFOpts, PaginateOpts};
use liboxen::view::{
    JsonDataFrameView, JsonDataFrameViewResponse, JsonDataFrameViews, Pagination, StatusMessage,
};

use liboxen::util;

pub async fn get(
    req: HttpRequest,
    query: web::Query<DFOptsQuery>,
) -> actix_web::Result<HttpResponse, OxenHttpError> {
    let app_data = app_data(&req)?;
    let namespace = path_param(&req, "namespace")?;
    let repo_name = path_param(&req, "repo_name")?;
    let repo = get_repo(&app_data.path, namespace, &repo_name)?;
    let resource = parse_resource(&req, &repo)?;

    log::debug!(
        "{} resource {}/{}",
        current_function!(),
        repo_name,
        resource
    );

    // Get the path to the versioned file on disk
    let version_path =
        util::fs::version_path_for_commit_id(&repo, &resource.commit.id, &resource.file_path)?;
    log::debug!(
        "controllers::data_frames Reading version file {:?}",
        version_path
    );

    // Get the cached size of the data frame
    let data_frame_size =
        cachers::df_size::get_cache_for_version(&repo, &resource.commit, &version_path)?;

    log::debug!(
        "controllers::data_frames got data frame size {:?}",
        data_frame_size
    );

    // Parse the query params
    let mut opts = DFOpts::empty();
    opts = df_opts_query::parse_opts(&query, &mut opts);
    log::debug!("controllers::data_frames got opts {:?}", opts);

    // Paginate or slice, after we do the original transform
    let mut page_opts = PaginateOpts {
        page_num: constants::DEFAULT_PAGE_NUM,
        page_size: constants::DEFAULT_PAGE_SIZE,
    };

    // If we have slice params, use them
    if let Some((start, end)) = opts.slice_indices() {
        log::debug!(
            "controllers::data_frames Got slice params {}..{}",
            start,
            end
        );
    } else {
        // Otherwise use the query params for pagination
        let page = query.page.unwrap_or(constants::DEFAULT_PAGE_NUM);
        let page_size = query.page_size.unwrap_or(constants::DEFAULT_PAGE_SIZE);

        page_opts.page_num = page;
        page_opts.page_size = page_size;

        // Must translate page params to slice params
        let start = if page == 0 { 0 } else { page_size * (page - 1) };
        let end = page_size * page;
        opts.slice = Some(format!("{}..{}", start, end));
    }

    let df = tabular::scan_df(&version_path, &opts, data_frame_size.height)?;

    // Try to get the schema from disk
    let og_schema = if let Some(schema) =
        api::local::schemas::get_by_path_from_ref(&repo, &resource.commit.id, &resource.file_path)?
    {
        schema
    } else {
        match df.schema() {
            Ok(schema) => Ok(Schema::from_polars(&schema.to_owned())),
            Err(e) => {
                log::error!("Error reading df: {}", e);
                Err(OxenHttpError::InternalServerError)
            }
        }?
    };

    log::debug!(
        "controllers::data_frames Done getting schema {:?}",
        version_path
    );

    // We have to run the query param transforms, then paginate separately
    let og_df_json = JsonDataFrameSource::from_df_size(&data_frame_size, &og_schema);

    log::debug!(
        "controllers::data_frames BEFORE TRANSFORM LAZY {}",
        data_frame_size.height
    );

    match tabular::transform_lazy(df, data_frame_size.height, opts.clone()) {
        Ok(df_view) => {
            log::debug!("controllers::data_frames DF view {:?}", df_view);

            let resource_version = ResourceVersion {
                path: resource.file_path.to_string_lossy().into(),
                version: resource.version().to_owned(),
            };

            // Have to do the pagination after the transform
            let view_height = if opts.has_filter_transform() {
                df_view.height()
            } else {
                data_frame_size.height
            };

            let total_pages = (view_height as f64 / page_opts.page_size as f64).ceil() as usize;

            let mut df = tabular::transform_slice(df_view, data_frame_size.height, opts.clone())?;

            let mut slice_schema = Schema::from_polars(&df.schema());
            log::debug!("OG schema {:?}", og_schema);
            log::debug!("Pre-Slice schema {:?}", slice_schema);
            slice_schema.update_metadata_from_schema(&og_schema);
            log::debug!("Slice schema {:?}", slice_schema);
            let opts_view = DFOptsView::from_df_opts(&opts);

            let response = JsonDataFrameViewResponse {
                status: StatusMessage::resource_found(),
                data_frame: JsonDataFrameViews {
                    source: og_df_json,
                    view: JsonDataFrameView {
                        schema: slice_schema,
                        size: DataFrameSize {
                            height: df.height(),
                            width: df.width(),
                        },
                        data: JsonDataFrameView::json_from_df(&mut df),
                        pagination: Pagination {
                            page_number: page_opts.page_num,
                            page_size: page_opts.page_size,
                            total_pages,
                            total_entries: view_height,
                        },
                        opts: opts_view,
                    },
                },
                commit: Some(resource.commit.clone()),
                resource: Some(resource_version),
                derived_resource: None,
            };
            Ok(HttpResponse::Ok().json(response))
        }
        Err(OxenError::SQLParseError(sql)) => {
            log::error!("Error parsing SQL: {}", sql);
            Err(OxenHttpError::SQLParseError(sql))
        }
        Err(e) => {
            log::error!("Error transforming df: {}", e);
            Err(OxenHttpError::InternalServerError)
        }
    }
}
